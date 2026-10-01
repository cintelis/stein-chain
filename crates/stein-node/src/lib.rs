//! The Stein Chain devnet node: one validator that admits transactions over
//! RPC, produces blocks on a timer, and persists them to a block log.
//!
//! What is real here: transaction validation, ML-DSA verification, block
//! production and full re-verification on restart. What is not yet: peer
//! networking and BFT consensus (SPEC §6, §7, §11).

pub mod mempool;
pub mod rpc;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use stein_crypto::{Hash, SecretKey, secret_key_from_seed_file};
use stein_state::State;
use stein_types::{Genesis, Transaction};

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

/// Why the node could not start or act.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// File I/O.
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Unreadable genesis.
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
}

/// Shared node state.
pub struct Node {
    /// Account state; write-locked only while a block is built.
    pub state: RwLock<State>,
    /// Pending transactions.
    pub mempool: Mutex<Mempool>,
    /// Block history.
    pub store: RwLock<Store>,
    /// Genesis hash, served so clients can tell networks apart.
    pub genesis_hash: Hash,
    validator: SecretKey,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Node {
    /// Opens the node in `data_dir`, replaying and re-verifying the block log.
    pub fn open(data_dir: &Path) -> Result<Arc<Self>, NodeError> {
        let genesis_text = std::fs::read_to_string(data_dir.join(files::GENESIS))?;
        let genesis: Genesis =
            serde_json::from_str(&genesis_text).map_err(|e| NodeError::Genesis(e.to_string()))?;
        let mut state =
            State::from_genesis(&genesis).map_err(|e| NodeError::Genesis(e.to_string()))?;
        let validator = secret_key_from_seed_file(&data_dir.join(files::VALIDATOR_SEED))
            .map_err(|e| NodeError::Key(e.to_string()))?;
        if !state
            .validators()
            .iter()
            .any(|v| v.key.address() == validator.address())
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
            validator,
        }))
    }

    /// Verifies `tx` against current state and queues it.
    pub fn submit(&self, tx: Transaction) -> Result<Hash, String> {
        let queued_key = self.mempool.lock().queued_key(&tx.body.sender);
        self.state
            .read()
            .check_transaction(&tx, queued_key.as_ref())
            .map_err(|e| e.to_string())?;
        self.mempool.lock().admit(tx).map_err(|e| e.to_string())
    }

    /// Builds, persists and indexes one block if there is anything to include.
    pub fn produce_block(&self) -> Result<Option<u64>, NodeError> {
        let max = self.state.read().params().max_block_txs as usize;
        let candidates = self.mempool.lock().candidates(&self.state.read(), max);
        if candidates.is_empty() {
            return Ok(None);
        }
        let mut state = self.state.write();
        let (block, rejected) = state
            .build_block(candidates, &self.validator, now_ms())
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
}

/// Default data directory.
pub fn default_data_dir() -> PathBuf {
    PathBuf::from(".stein-devnet")
}
