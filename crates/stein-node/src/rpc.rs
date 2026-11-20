//! JSON RPC over HTTP. Devnet only: binds to localhost by default, and has no
//! authentication or rate limiting yet (SPEC §9.4).

use std::sync::Arc;

use axum::extract::{Path, Query, State as AxumState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use stein_crypto::{Address, Hash};
use stein_state::State;
use stein_types::{
    AMOUNT_DECIMALS, Amount, Asset, ChainId, Pool, SWAP_FEE_PER_MILLE, Token, Transaction,
    format_amount, format_units,
};

use crate::views::{BlockView, TxView};
use crate::{Node, Role, SYNC_BATCH};

/// Largest request body accepted: one transaction with a key is ~5.4 KB, hex
/// doubles it.
const MAX_BODY: usize = 64 * 1024;

/// The RPC router.
pub fn router(node: Arc<Node>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/account/{address}", get(account))
        .route("/tx", post(submit))
        .route("/tx/{id}", get(tx_status))
        .route("/block/{height}", get(block))
        .route("/genesis", get(genesis))
        .route("/raw/blocks", get(raw_blocks))
        .route("/blocks", get(blocks))
        .route("/mempool", get(mempool))
        .route("/account/{address}/txs", get(account_txs))
        .route("/events", get(events))
        .route("/validators", get(validators))
        .route("/validator/{address}", get(validator))
        .route("/accounts/top", get(top_accounts))
        .route("/tokens", get(tokens))
        .route("/token/{address}", get(token))
        .route("/pools", get(pools))
        .route("/pool/{address}", get(pool))
        // Explorers are served from their own origin. Reads are public; the
        // only write, POST /tx, carries its own signature.
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
                .allow_headers([axum::http::header::CONTENT_TYPE]),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(node)
}

fn error(code: StatusCode, message: impl ToString) -> Response {
    (code, Json(json!({ "error": message.to_string() }))).into_response()
}

/// `GET /status`.
#[derive(Serialize, Deserialize)]
pub struct Status {
    /// Network id.
    pub chain_id: ChainId,
    /// Genesis hash.
    pub genesis_hash: Hash,
    /// Latest height.
    pub height: u64,
    /// Latest block hash.
    pub head: Hash,
    /// Latest state root.
    pub state_root: Hash,
    /// Queued transactions.
    pub mempool: usize,
    /// Fee rule: minimum base units per encoded byte.
    pub min_fee_per_byte: Amount,
    /// Expiry rule.
    pub max_tx_ttl_blocks: u64,
    /// `validator` or `follower`.
    #[serde(default)]
    pub role: String,
    /// The validator a follower follows.
    #[serde(default)]
    pub leader: Option<String>,
    /// Why a follower stopped following, if it did.
    #[serde(default)]
    pub sync_error: Option<String>,
    /// Signature schemes accounts may use on this network.
    #[serde(default)]
    pub tx_schemes: Vec<stein_crypto::Scheme>,
    /// Transactions in all blocks this node holds.
    #[serde(default)]
    pub tx_total: u64,
    /// Accounts that exist.
    #[serde(default)]
    pub accounts: usize,
    /// Total supply in STEIN (fixed at genesis on the devnet).
    #[serde(default)]
    pub total_supply_stein: String,
    /// Target time between blocks.
    #[serde(default)]
    pub block_time_ms: u64,
    /// Shard this node serves. Always 0 until sharding (SHARD-READY I-8).
    #[serde(default)]
    pub shard_id: u16,
    /// The system account that collects fees until each epoch's payout.
    #[serde(default)]
    pub fee_pool: Option<Address>,
    /// Blocks per epoch (fee pool payout interval).
    #[serde(default)]
    pub epoch_blocks: u64,
    /// Tokens issued.
    #[serde(default)]
    pub tokens: usize,
    /// Liquidity pools.
    #[serde(default)]
    pub pools: usize,
}

async fn status(AxumState(node): AxumState<Arc<Node>>) -> Json<Status> {
    let state = node.state.read();
    Json(Status {
        chain_id: state.chain_id(),
        genesis_hash: node.genesis_hash,
        height: state.height(),
        head: state.head(),
        state_root: state.state_root(),
        mempool: node.mempool.lock().len(),
        min_fee_per_byte: state.params().min_fee_per_byte,
        max_tx_ttl_blocks: state.params().max_tx_ttl_blocks,
        role: match node.role {
            Role::Validator(_) => "validator".into(),
            Role::Follower(_) => "follower".into(),
        },
        leader: match &node.role {
            Role::Follower(l) => Some(l.url.clone()),
            Role::Validator(_) => None,
        },
        sync_error: node.sync_error.lock().clone(),
        tx_schemes: state.params().tx_schemes.clone(),
        tx_total: node.store.read().tx_total(),
        accounts: state.account_count(),
        total_supply_stein: format_amount(node.genesis.total_supply().unwrap_or(0)),
        block_time_ms: state.params().block_time_ms,
        shard_id: 0,
        fee_pool: Some(stein_types::fee_pool()),
        epoch_blocks: state.params().epoch_blocks,
        tokens: state.tokens().count(),
        pools: state.pools().count(),
    })
}

/// `GET /account/{address}`.
#[derive(Serialize, Deserialize)]
pub struct AccountView {
    /// Address.
    pub address: Address,
    /// Balance in base units, as a string (u128 does not fit JSON numbers).
    pub balance: String,
    /// Balance in STEIN.
    pub balance_stein: String,
    /// Next nonce.
    pub nonce: u64,
    /// Whether the account's public key is registered on chain.
    pub key_registered: bool,
    /// What the address is: `account`, `token` or `pool`.
    #[serde(default)]
    pub kind: String,
    /// Tokens and pool shares the address holds.
    #[serde(default)]
    pub holdings: Vec<HoldingView>,
}

/// A balance of a token or of a pool's shares.
#[derive(Serialize, Deserialize)]
pub struct HoldingView {
    /// The token's address, or the pool's.
    pub asset: Address,
    /// Its symbol; for shares, the pair.
    pub symbol: String,
    /// Decimal places.
    pub decimals: u32,
    /// Whether these are shares of a pool.
    pub pool_shares: bool,
    /// Balance in base units (string).
    pub balance: String,
    /// Balance in whole units.
    pub balance_formatted: String,
}

/// How an asset is shown: its symbol and decimal places. A pool's shares are
/// named after its pair and counted in whole units.
fn label(state: &State, asset: &Asset) -> (String, u32) {
    match asset {
        Asset::Stein => ("STEIN".into(), AMOUNT_DECIMALS),
        Asset::Token(address) => match (state.token(address), state.pool(address)) {
            (Some(token), _) => (token.symbol.clone(), u32::from(token.decimals)),
            (None, Some(pool)) => {
                let (a, _) = label(state, &pool.asset_a);
                let (b, _) = label(state, &pool.asset_b);
                (format!("{a}/{b} shares"), 0)
            }
            (None, None) => ("?".into(), 0),
        },
    }
}

fn account_view(state: &State, address: Address) -> AccountView {
    let account = state.account(&address).cloned().unwrap_or_default();
    let holdings = state
        .holdings_of(&address)
        .into_iter()
        .map(|(asset, balance)| {
            let (symbol, decimals) = label(state, &Asset::Token(asset));
            HoldingView {
                asset,
                symbol,
                decimals,
                pool_shares: state.pool(&asset).is_some(),
                balance: balance.to_string(),
                balance_formatted: format_units(balance, decimals),
            }
        })
        .collect();
    let kind = if state.token(&address).is_some() {
        "token"
    } else if state.pool(&address).is_some() {
        "pool"
    } else {
        "account"
    };
    AccountView {
        address,
        balance: account.balance.to_string(),
        balance_stein: format_amount(account.balance),
        nonce: account.nonce,
        key_registered: account.public_key.is_some(),
        kind: kind.into(),
        holdings,
    }
}

async fn account(AxumState(node): AxumState<Arc<Node>>, Path(address): Path<String>) -> Response {
    let address: Address = match address.parse() {
        Ok(a) => a,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    Json(account_view(&node.state.read(), address)).into_response()
}

/// A token, as `GET /token/{address}` and `GET /tokens` show it.
#[derive(Serialize, Deserialize)]
pub struct TokenView {
    /// Its address.
    pub address: Address,
    /// The account that issued it.
    pub issuer: Address,
    /// Display label; not unique.
    pub symbol: String,
    /// Decimal places.
    pub decimals: u32,
    /// Total supply in base units (string). Fixed.
    pub supply: String,
    /// Total supply in whole units.
    pub supply_formatted: String,
}

fn token_view(address: &Address, token: &Token) -> TokenView {
    let decimals = u32::from(token.decimals);
    TokenView {
        address: *address,
        issuer: token.issuer,
        symbol: token.symbol.clone(),
        decimals,
        supply: token.supply.to_string(),
        supply_formatted: format_units(token.supply, decimals),
    }
}

/// `GET /tokens?limit=N`: tokens, in address order.
async fn tokens(
    AxumState(node): AxumState<Arc<Node>>,
    Query(q): Query<ListQuery>,
) -> Json<Vec<TokenView>> {
    let state = node.state.read();
    Json(
        state
            .tokens()
            .take(q.limit.min(MAX_LIST))
            .map(|(address, token)| token_view(address, token))
            .collect(),
    )
}

/// `GET /token/{address}`.
async fn token(AxumState(node): AxumState<Arc<Node>>, Path(address): Path<String>) -> Response {
    let address: Address = match address.parse() {
        Ok(a) => a,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match node.state.read().token(&address) {
        Some(token) => Json(token_view(&address, token)).into_response(),
        None => error(StatusCode::NOT_FOUND, "no such token"),
    }
}

/// A liquidity pool, as `GET /pool/{address}` and `GET /pools` show it.
#[derive(Serialize, Deserialize)]
pub struct PoolView {
    /// Its address; also the asset its shares are held under.
    pub address: Address,
    /// First asset of the pair: `stein` or a token address.
    pub asset_a: Asset,
    /// Second asset.
    pub asset_b: Asset,
    /// The first asset's symbol.
    pub symbol_a: String,
    /// The second asset's symbol.
    pub symbol_b: String,
    /// The first asset's decimal places.
    pub decimals_a: u32,
    /// The second asset's decimal places.
    pub decimals_b: u32,
    /// Reserve of the first asset, in base units (string).
    pub reserve_a: String,
    /// Reserve of the second asset, in base units (string).
    pub reserve_b: String,
    /// Reserve of the first asset, in whole units.
    pub reserve_a_formatted: String,
    /// Reserve of the second asset, in whole units.
    pub reserve_b_formatted: String,
    /// Shares in existence (string), the locked minimum included.
    pub shares: String,
    /// The fee a swap leaves in the pool, in thousandths of what is paid in.
    pub fee_per_mille: u32,
}

fn pool_view(state: &State, address: &Address, pool: &Pool) -> PoolView {
    let (symbol_a, decimals_a) = label(state, &pool.asset_a);
    let (symbol_b, decimals_b) = label(state, &pool.asset_b);
    let reserve_a = state.balance_of(address, &pool.asset_a);
    let reserve_b = state.balance_of(address, &pool.asset_b);
    PoolView {
        address: *address,
        asset_a: pool.asset_a,
        asset_b: pool.asset_b,
        symbol_a,
        symbol_b,
        decimals_a,
        decimals_b,
        reserve_a: reserve_a.to_string(),
        reserve_b: reserve_b.to_string(),
        reserve_a_formatted: format_units(reserve_a, decimals_a),
        reserve_b_formatted: format_units(reserve_b, decimals_b),
        shares: pool.shares.to_string(),
        fee_per_mille: SWAP_FEE_PER_MILLE as u32,
    }
}

/// `GET /pools?limit=N`: pools, in address order.
async fn pools(
    AxumState(node): AxumState<Arc<Node>>,
    Query(q): Query<ListQuery>,
) -> Json<Vec<PoolView>> {
    let state = node.state.read();
    Json(
        state
            .pools()
            .take(q.limit.min(MAX_LIST))
            .map(|(address, pool)| pool_view(&state, address, pool))
            .collect(),
    )
}

/// `GET /pool/{address}`.
async fn pool(AxumState(node): AxumState<Arc<Node>>, Path(address): Path<String>) -> Response {
    let address: Address = match address.parse() {
        Ok(a) => a,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let state = node.state.read();
    match state.pool(&address) {
        Some(pool) => Json(pool_view(&state, &address, pool)).into_response(),
        None => error(StatusCode::NOT_FOUND, "no such pool"),
    }
}

/// `POST /tx` body: the Borsh-encoded transaction, hex.
#[derive(Serialize, Deserialize)]
pub struct SubmitRequest {
    /// Hex of the Borsh encoding.
    pub raw: String,
}

async fn submit(AxumState(node): AxumState<Arc<Node>>, Json(req): Json<SubmitRequest>) -> Response {
    let tx = match hex::decode(&req.raw)
        .ok()
        .and_then(|b| borsh::from_slice::<Transaction>(&b).ok())
    {
        Some(tx) => tx,
        None => {
            return error(
                StatusCode::BAD_REQUEST,
                "raw is not a hex-encoded transaction",
            );
        }
    };
    match node.submit(tx).await {
        Ok(id) => Json(json!({ "id": id })).into_response(),
        Err(why) => error(StatusCode::UNPROCESSABLE_ENTITY, why),
    }
}

async fn tx_status(AxumState(node): AxumState<Arc<Node>>, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<Hash>() else {
        return error(StatusCode::BAD_REQUEST, "bad transaction id");
    };
    let store = node.store.read();
    if let Some(loc) = store.locate(&id) {
        let tx = store.block(loc.height).and_then(|b| {
            let body = b.bodies.get(loc.index as usize)?;
            let witness = b.witnesses.as_ref().and_then(|w| w.get(loc.index as usize));
            Some(TxView::of(
                body,
                witness,
                Some((loc.height, loc.index)),
                store.outcome(&id),
            ))
        });
        return Json(
            json!({ "status": "included", "height": loc.height, "index": loc.index, "tx": tx }),
        )
        .into_response();
    }
    drop(store);
    match node.mempool.lock().get(&id) {
        Some(tx) => Json(json!({ "status": "pending", "tx": TxView::pending(tx) })).into_response(),
        None => Json(json!({ "status": "unknown" })).into_response(),
    }
}

#[derive(Deserialize)]
struct BlockQuery {
    #[serde(default)]
    witnesses: bool,
}

async fn block(
    AxumState(node): AxumState<Arc<Node>>,
    Path(height): Path<u64>,
    Query(q): Query<BlockQuery>,
) -> Response {
    let store = node.store.read();
    let Some(block) = store.block(height) else {
        return error(StatusCode::NOT_FOUND, "no such block");
    };
    let mut body = json!({
        "hash": block.hash(),
        "header": block.header,
        "bodies": block.bodies,
        "witnesses_available": block.witnesses.is_some(),
    });
    if q.witnesses {
        body["witnesses"] = json!(block.witnesses);
    }
    Json(body).into_response()
}

async fn genesis(AxumState(node): AxumState<Arc<Node>>) -> Json<stein_types::Genesis> {
    Json(node.genesis.clone())
}

#[derive(Deserialize)]
struct RangeQuery {
    from: u64,
    #[serde(default = "default_max")]
    max: u64,
}

fn default_max() -> u64 {
    SYNC_BATCH
}

/// Most bytes of blocks served per request.
const MAX_RANGE_BYTES: usize = 32 * 1024 * 1024;

/// `GET /raw/blocks?from=H&max=N`: Borsh `Vec<Block>`, witnesses always
/// included (read from the block log once pruned from memory). What followers
/// sync from.
async fn raw_blocks(
    AxumState(node): AxumState<Arc<Node>>,
    Query(q): Query<RangeQuery>,
) -> Response {
    // Old blocks are read from disk: keep that off the async workers.
    let encoded = tokio::task::spawn_blocking(move || {
        let store = node.store.read();
        store
            .range(q.from, q.max.min(SYNC_BATCH), MAX_RANGE_BYTES)
            .and_then(|blocks| borsh::to_vec(&blocks))
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e)));
    match encoded {
        Ok(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    /// Newest height to include (default: the tip).
    before: Option<u64>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

/// Most items any list endpoint returns.
const MAX_LIST: usize = 100;

/// `GET /blocks?before=H&limit=N`: recent blocks, newest first.
async fn blocks(
    AxumState(node): AxumState<Arc<Node>>,
    Query(q): Query<ListQuery>,
) -> Json<Vec<BlockView>> {
    let store = node.store.read();
    let top = q.before.unwrap_or_else(|| store.tip()).min(store.tip());
    let views = (1..=top)
        .rev()
        .take(q.limit.min(MAX_LIST))
        .filter_map(|h| store.block(h))
        .map(|b| BlockView::of(b, &store.outcomes_for(b)))
        .collect();
    Json(views)
}

/// `GET /mempool?limit=N`: transactions waiting for a block.
async fn mempool(
    AxumState(node): AxumState<Arc<Node>>,
    Query(q): Query<ListQuery>,
) -> Json<Vec<TxView>> {
    let pool = node.mempool.lock();
    Json(
        pool.pending(q.limit.min(MAX_LIST))
            .into_iter()
            .map(TxView::pending)
            .collect(),
    )
}

/// `GET /account/{address}/txs?limit=N`: transactions touching an account,
/// newest first.
async fn account_txs(
    AxumState(node): AxumState<Arc<Node>>,
    Path(address): Path<String>,
    Query(q): Query<ListQuery>,
) -> Response {
    let address: Address = match address.parse() {
        Ok(a) => a,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let store = node.store.read();
    let views: Vec<TxView> = store
        .address_txs(&address, q.limit.min(MAX_LIST))
        .into_iter()
        .filter_map(|loc| {
            let b = store.block(loc.height)?;
            let body = b.bodies.get(loc.index as usize)?;
            let witness = b.witnesses.as_ref().and_then(|w| w.get(loc.index as usize));
            Some(TxView::of(
                body,
                witness,
                Some((loc.height, loc.index)),
                store.outcome(&body.id()),
            ))
        })
        .collect();
    Json(views).into_response()
}

/// `GET /events`: server-sent events, one JSON `Event` per message, as blocks
/// are produced or verified and transactions accepted.
async fn events(
    AxumState(node): AxumState<Arc<Node>>,
) -> axum::response::sse::Sse<
    impl futures_core::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio_stream::StreamExt as _;
    use tokio_stream::wrappers::BroadcastStream;

    let stream = BroadcastStream::new(node.events.subscribe()).filter_map(|event| {
        // A subscriber that fell behind skips what it missed; it can catch
        // up through GET /blocks.
        let event = event.ok()?;
        let kind = match &event {
            crate::views::Event::Block(_) => "block",
            crate::views::Event::Pending(_) => "pending",
        };
        Event::default().event(kind).json_data(&event).ok().map(Ok)
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /validators`: the active validator set.
async fn validators(AxumState(node): AxumState<Arc<Node>>) -> Json<serde_json::Value> {
    let state = node.state.read();
    let total: u64 = state
        .validators()
        .iter()
        .map(|v| v.power)
        .fold(0u64, u64::saturating_add);
    let list: Vec<serde_json::Value> = state
        .validators()
        .iter()
        .map(|v| {
            let (blocks_proposed, _) = node.store.read().proposed(&v.key.address(), 0);
            json!({
                "address": v.key.address(),
                "scheme": v.key.scheme(),
                "power": v.power,
                "share": if total == 0 { 0.0 } else { v.power as f64 / total as f64 },
                "blocks_proposed": blocks_proposed,
            })
        })
        .collect();
    Json(json!({ "validators": list, "total_power": total }))
}

/// `GET /accounts/top?limit=N`: accounts by balance, largest first.
async fn top_accounts(
    AxumState(node): AxumState<Arc<Node>>,
    Query(q): Query<ListQuery>,
) -> Json<Vec<AccountView>> {
    let state = node.state.read();
    Json(
        state
            .richest(q.limit.min(MAX_LIST))
            .into_iter()
            .map(|(address, _)| account_view(&state, address))
            .collect(),
    )
}

/// `GET /validator/{address}?limit=N`: whether `address` is an active
/// validator, its consensus key and power, and the blocks it proposed
/// (newest first). `validator` is null for any other address.
async fn validator(
    AxumState(node): AxumState<Arc<Node>>,
    Path(address): Path<String>,
    Query(q): Query<ListQuery>,
) -> Response {
    let address: Address = match address.parse() {
        Ok(a) => a,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let state = node.state.read();
    let total: u64 = state
        .validators()
        .iter()
        .map(|v| v.power)
        .fold(0u64, u64::saturating_add);
    let entry = state
        .validators()
        .iter()
        .find(|v| v.key.address() == address)
        .map(|v| {
            json!({
                "scheme": v.key.scheme(),
                "power": v.power,
                "share": if total == 0 { 0.0 } else { v.power as f64 / total as f64 },
                "epoch_blocks": state.params().epoch_blocks,
            })
        });
    drop(state);
    let store = node.store.read();
    let (count, heights) = store.proposed(&address, q.limit.min(MAX_LIST));
    let blocks: Vec<BlockView> = heights
        .into_iter()
        .filter_map(|h| store.block(h))
        .map(|b| BlockView::of(b, &store.outcomes_for(b)))
        .collect();
    Json(json!({ "validator": entry, "blocks_proposed": count, "blocks": blocks })).into_response()
}
