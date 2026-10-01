//! The Stein Chain devnet node, in one of two roles:
//!
//! * **Validator** — admits transactions over RPC, produces blocks on a timer
//!   and persists them to a block log.
//! * **Follower** — forwards transactions it receives to a validator, and
//!   downloads the validator's blocks over HTTPS. It re-verifies every
//!   signature and the state root before applying a block: it trusts the
//!   leader for *nothing* except availability (SPEC §5.2). A follower pins the
//!   genesis hash it was given, so a leader on another chain is refused.
//!
//! Following over HTTPS is the devnet stand-in for phase 3's QUIC gossip and
//! BFT consensus (SPEC §6, §7). The verification rules are the same.

pub mod mempool;
pub mod rpc;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use stein_crypto::{Hash, SecretKey, secret_key_from_seed_file};
use stein_state::State;
use stein_types::{Block, Genesis, Transaction};

use crate::mempool::{Limits, Mempool};
use crate::store::Store;

/// File names inside a node's data directory.
pub mod files {
    /// The genesis document.
    pub const GENESIS: &str = "genesis.json";
    /// The validator's ML-DSA seed.
    pub const VALIDATOR_SEED: &str = "validator.seed";
    /// The block log.
    pub const BLOCK_LOG: &str = "blocks.log";
}

/// Most blocks a follower asks for at once.
pub const SYNC_BATCH: u64 = 100;

/// Why the node could not start or act.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// File I/O.
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Unreadable or unexpected genesis.
    #[error("genesis: {0}")]
    Genesis(String),
    /// Unusable key.
    #[error("key: {0}")]
    Key(String),
    /// The block log does not replay: corrupted or from another chain.
    #[error("block log does not replay at height {height}: {reason}")]
    Replay {
        /// Failing height.
        height: u64,
        /// Why.
        reason: String,
    },
    /// Talking to the leader failed.
    #[error("leader: {0}")]
    Leader(String),
    /// The leader served something this node must never apply. Following
    /// stops; the node keeps serving what it has already verified.
    #[error("refused leader data: {0}")]
    Refused(String),
}

/// The validator this node follows.
pub struct Leader {
    /// Base URL of the leader's RPC.
    pub url: String,
    client: reqwest::Client,
}

/// What this node does.
pub enum Role {
    /// Produces blocks with this key.
    Validator(SecretKey),
    /// Follows a validator.
    Follower(Leader),
}

/// Shared node state.
pub struct Node {
    /// Account state; write-locked only while a block is built or imported.
    pub state: RwLock<State>,
    /// Pending transactions. A validator builds blocks from them; a follower
    /// only remembers what it forwarded until the blocks arrive.
    pub mempool: Mutex<Mempool>,
    /// Block history.
    pub store: RwLock<Store>,
    /// The genesis document.
    pub genesis: Genesis,
    /// Genesis hash, served so clients can tell networks apart.
    pub genesis_hash: Hash,
    /// Validator or follower.
    pub role: Role,
    /// Why following stopped, if it did.
    pub sync_error: Mutex<Option<String>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn read_genesis(path: &Path) -> Result<Genesis, NodeError> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| NodeError::Genesis(e.to_string()))
}

impl Node {
    /// Opens a validator in `data_dir`, replaying and re-verifying its log.
    pub fn open(data_dir: &Path) -> Result<Arc<Self>, NodeError> {
        let genesis = read_genesis(&data_dir.join(files::GENESIS))?;
        let validator = secret_key_from_seed_file(&data_dir.join(files::VALIDATOR_SEED))
            .map_err(|e| NodeError::Key(e.to_string()))?;
        Self::open_with(data_dir, genesis, Role::Validator(validator))
    }

    /// Opens a follower of the validator at `leader_url`. The genesis document
    /// is read from `data_dir`, or fetched from the leader on first start; in
    /// both cases its hash must equal `pinned_genesis`.
    pub async fn open_follower(
        data_dir: &Path,
        leader_url: &str,
        pinned_genesis: Hash,
    ) -> Result<Arc<Self>, NodeError> {
        let tls = stein_tls::client_config().map_err(|e| NodeError::Leader(e.to_string()))?;
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| NodeError::Leader(e.to_string()))?;
        let url = leader_url.trim_end_matches('/').to_string();

        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(files::GENESIS);
        let genesis = if path.exists() {
            read_genesis(&path)?
        } else {
            let genesis: Genesis = client
                .get(format!("{url}/genesis"))
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|e| NodeError::Leader(e.to_string()))?
                .json()
                .await
                .map_err(|e| NodeError::Leader(e.to_string()))?;
            genesis
        };
        if genesis.hash() != pinned_genesis {
            return Err(NodeError::Genesis(format!(
                "genesis hash {} is not the pinned {pinned_genesis}",
                genesis.hash()
            )));
        }
        if !path.exists() {
            let text = serde_json::to_string_pretty(&genesis)
                .map_err(|e| NodeError::Genesis(e.to_string()))?;
            std::fs::write(&path, text)?;
        }
        Self::open_with(data_dir, genesis, Role::Follower(Leader { url, client }))
    }

    fn open_with(data_dir: &Path, genesis: Genesis, role: Role) -> Result<Arc<Self>, NodeError> {
        let mut state =
            State::from_genesis(&genesis).map_err(|e| NodeError::Genesis(e.to_string()))?;
        if let Role::Validator(key) = &role
            && !state
                .validators()
                .iter()
                .any(|v| v.key.address() == key.address())
        {
            return Err(NodeError::Key(
                "validator key is not in the genesis validator set".into(),
            ));
        }

        let retention = genesis.params.witness_retention_blocks;
        let (mut store, recovered) = Store::open(&data_dir.join(files::BLOCK_LOG), retention)?;
        for block in recovered {
            let height = block.header.height;
            state.import_block(&block).map_err(|e| NodeError::Replay {
                height,
                reason: e.to_string(),
            })?;
            store.append(block, false)?;
        }
        tracing::info!(
            height = state.height(),
            "replayed and re-verified block log"
        );

        Ok(Arc::new(Self {
            state: RwLock::new(state),
            mempool: Mutex::new(Mempool::new(Limits::default())),
            store: RwLock::new(store),
            genesis_hash: genesis.hash(),
            genesis,
            role,
            sync_error: Mutex::new(None),
        }))
    }

    /// Verifies `tx` against current state, then queues it (validator) or
    /// forwards it to the leader (follower). A follower checks first, so junk
    /// is refused at the edge and the leader's verification hits a warm cache
    /// nowhere but its own.
    pub async fn submit(self: &Arc<Self>, tx: Transaction) -> Result<Hash, String> {
        let node = Arc::clone(self);
        let checked = tokio::task::spawn_blocking(move || {
            let queued_key = node.mempool.lock().queued_key(&tx.body.sender);
            node.state
                .read()
                .check_transaction(&tx, queued_key.as_ref())
                .map(|()| tx)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;

        match &self.role {
            Role::Validator(_) => self
                .mempool
                .lock()
                .admit(checked)
                .map_err(|e| e.to_string()),
            Role::Follower(leader) => {
                let id = leader.forward(&checked).await?;
                // Remembered locally (never produced from), so a follow-up from
                // a new account verifies against the key its queued first
                // transaction registers. Pruned as blocks arrive.
                let _ = self.mempool.lock().admit(checked);
                Ok(id)
            }
        }
    }

    /// Builds, persists and indexes one block if there is anything to include.
    /// Validators only.
    pub fn produce_block(&self) -> Result<Option<u64>, NodeError> {
        let Role::Validator(key) = &self.role else {
            return Err(NodeError::Key("a follower does not produce blocks".into()));
        };
        let max = self.state.read().params().max_block_txs as usize;
        let candidates = self.mempool.lock().candidates(&self.state.read(), max);
        if candidates.is_empty() {
            return Ok(None);
        }
        let mut state = self.state.write();
        let (block, rejected) = state
            .build_block(candidates, key, now_ms())
            .map_err(|e| NodeError::Key(e.to_string()))?;
        for (id, why) in &rejected {
            tracing::debug!(%id, %why, "left out of block");
        }
        let height = block.header.height;
        let count = block.bodies.len();
        // Persist before anyone can observe the new state through RPC.
        self.store.write().append(block, true)?;
        let rejected: Vec<Hash> = rejected.into_iter().map(|(id, _)| id).collect();
        self.mempool.lock().prune(&state, &rejected);
        drop(state);
        tracing::info!(height, txs = count, "produced block");
        Ok(Some(height))
    }

    /// Produces blocks every `block_time` until the task is cancelled.
    pub async fn run_producer(self: Arc<Self>) {
        let block_time = Duration::from_millis(self.state.read().params().block_time_ms);
        let mut ticker = tokio::time::interval(block_time);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let node = Arc::clone(&self);
            match tokio::task::spawn_blocking(move || node.produce_block()).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    // A block that cannot be persisted must not be built on.
                    tracing::error!(%e, "block production failed; stopping");
                    std::process::exit(1);
                }
                Err(e) => tracing::error!(%e, "producer task panicked"),
            }
        }
    }

    /// One round of following: fetches up to [`SYNC_BATCH`] blocks the leader
    /// has and this node does not, and imports them with full verification.
    /// Returns how many blocks were applied.
    pub async fn sync_once(self: &Arc<Self>) -> Result<u64, NodeError> {
        let Role::Follower(leader) = &self.role else {
            return Ok(0);
        };
        let status: rpc::Status = leader.get_json("/status").await?;
        if status.genesis_hash != self.genesis_hash {
            return Err(NodeError::Refused(format!(
                "leader is on genesis {}, this node on {}",
                status.genesis_hash, self.genesis_hash
            )));
        }
        let from = self.state.read().height().saturating_add(1);
        if status.height < from {
            return Ok(0);
        }
        let bytes = leader
            .get_bytes(&format!("/raw/blocks?from={from}&max={SYNC_BATCH}"))
            .await?;
        let blocks: Vec<Block> = borsh::from_slice(&bytes)
            .map_err(|e| NodeError::Refused(format!("undecodable blocks: {e}")))?;

        let node = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let mut applied = 0u64;
            for block in blocks {
                let height = block.header.height;
                node.state
                    .write()
                    .import_block(&block)
                    .map_err(|e| NodeError::Refused(format!("block {height}: {e}")))?;
                node.store.write().append(block, true)?;
                applied = applied.saturating_add(1);
                tracing::info!(height, "imported and verified block");
            }
            node.mempool.lock().prune(&node.state.read(), &[]);
            Ok(applied)
        })
        .await
        .map_err(|e| NodeError::Leader(e.to_string()))?
    }

    /// Follows the leader until the task is cancelled. Network trouble is
    /// retried; data that fails verification stops following for good.
    pub async fn run_follower(self: Arc<Self>) {
        loop {
            match self.sync_once().await {
                Ok(0) => tokio::time::sleep(Duration::from_millis(500)).await,
                Ok(_) => {}
                Err(NodeError::Refused(why)) => {
                    tracing::error!(%why, "leader served data that does not verify; following stopped");
                    *self.sync_error.lock() = Some(why);
                    return;
                }
                Err(e) => {
                    tracing::warn!(%e, "sync failed; retrying");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
}

impl Leader {
    async fn get(&self, path: &str) -> Result<reqwest::Response, NodeError> {
        self.client
            .get(format!("{}{path}", self.url))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| NodeError::Leader(e.to_string()))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, NodeError> {
        self.get(path)
            .await?
            .json()
            .await
            .map_err(|e| NodeError::Leader(e.to_string()))
    }

    async fn get_bytes(&self, path: &str) -> Result<Vec<u8>, NodeError> {
        let bytes = self.get(path).await?.bytes().await;
        bytes
            .map(|b| b.to_vec())
            .map_err(|e| NodeError::Leader(e.to_string()))
    }

    async fn forward(&self, tx: &Transaction) -> Result<Hash, String> {
        let raw = hex::encode(borsh::to_vec(tx).map_err(|e| e.to_string())?);
        let response = self
            .client
            .post(format!("{}/tx", self.url))
            .json(&rpc::SubmitRequest { raw })
            .send()
            .await
            .map_err(|e| format!("leader unreachable: {e}"))?;
        let ok = response.status().is_success();
        let reply: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
        if !ok {
            return Err(format!("leader rejected: {}", reply["error"]));
        }
        serde_json::from_value(reply["id"].clone()).map_err(|e| e.to_string())
    }
}

/// Default data directory.
pub fn default_data_dir() -> PathBuf {
    PathBuf::from(".stein-devnet")
}
