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
use stein_types::{Amount, ChainId, Transaction, format_amount};

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
}

async fn account(AxumState(node): AxumState<Arc<Node>>, Path(address): Path<String>) -> Response {
    let address: Address = match address.parse() {
        Ok(a) => a,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let state = node.state.read();
    let account = state.account(&address).cloned().unwrap_or_default();
    Json(AccountView {
        address,
        balance: account.balance.to_string(),
        balance_stein: format_amount(account.balance),
        nonce: account.nonce,
        key_registered: account.public_key.is_some(),
    })
    .into_response()
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
            Some(TxView::of(body, witness, Some((loc.height, loc.index))))
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

/// `GET /raw/blocks?from=H&max=N`: Borsh `Vec<Block>`, witnesses included
/// while retained. What followers sync from.
async fn raw_blocks(
    AxumState(node): AxumState<Arc<Node>>,
    Query(q): Query<RangeQuery>,
) -> Response {
    let encoded = {
        let store = node.store.read();
        borsh::to_vec(&store.range(q.from, q.max.min(SYNC_BATCH), MAX_RANGE_BYTES))
    };
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
        .filter_map(|h| store.block(h).map(BlockView::of))
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
            Some(TxView::of(body, witness, Some((loc.height, loc.index))))
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
