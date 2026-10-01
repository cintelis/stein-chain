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
    match node.store.read().locate(&id) {
        Some(loc) => {
            Json(json!({ "status": "included", "height": loc.height, "index": loc.index }))
                .into_response()
        }
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
