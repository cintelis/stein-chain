//! Pending transactions, per sender and ordered by nonce.

use std::collections::{BTreeMap, HashMap, HashSet};

use stein_crypto::{Address, Hash, PublicKey};
use stein_state::State;
use stein_types::Transaction;

/// Mempool limits. Bounded so that a flood of valid-but-unminable
/// transactions cannot exhaust memory.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Most transactions held in total.
    pub max_total: usize,
    /// Most queued transactions per sender.
    pub max_per_sender: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total: 100_000,
            max_per_sender: 64,
        }
    }
}

/// Why a verified transaction was still not queued.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmitError {
    /// Already queued (same body).
    #[error("already in the mempool")]
    Duplicate,
    /// Another transaction with this sender and nonce is queued.
    #[error("nonce already used by a queued transaction")]
    NonceTaken,
    /// Mempool or sender queue is full.
    #[error("mempool full")]
    Full,
}

/// The mempool.
#[derive(Default)]
pub struct Mempool {
    limits: Limits,
    by_sender: HashMap<Address, BTreeMap<u64, Transaction>>,
    ids: HashSet<Hash>,
}

impl Mempool {
    /// An empty pool.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    /// Number of queued transactions.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The public key attached to `sender`'s queued key-registering
    /// transaction, if one is queued.
    pub fn queued_key(&self, sender: &Address) -> Option<PublicKey> {
        self.by_sender
            .get(sender)?
            .values()
            .find_map(|tx| tx.public_key().cloned())
    }

    /// Queues an already-verified transaction.
    pub fn admit(&mut self, tx: Transaction) -> Result<Hash, AdmitError> {
        let id = tx.id();
        if self.ids.contains(&id) {
            return Err(AdmitError::Duplicate);
        }
        if self.ids.len() >= self.limits.max_total {
            return Err(AdmitError::Full);
        }
        let queue = self.by_sender.entry(tx.body.sender).or_default();
        if queue.contains_key(&tx.body.nonce) {
            return Err(AdmitError::NonceTaken);
        }
        if queue.len() >= self.limits.max_per_sender {
            return Err(AdmitError::Full);
        }
        queue.insert(tx.body.nonce, tx);
        self.ids.insert(id);
        Ok(id)
    }

    /// Candidates for the next block: for each sender, the run of consecutive
    /// nonces starting at the account's next nonce, up to `max` in total.
    pub fn candidates(&self, state: &State, max: usize) -> Vec<Transaction> {
        let mut out = Vec::new();
        for (sender, queue) in &self.by_sender {
            let mut next = state.account(sender).map_or(0, |a| a.nonce);
            for (nonce, tx) in queue.range(next..) {
                if *nonce != next || out.len() >= max {
                    break;
                }
                out.push(tx.clone());
                next = next.saturating_add(1);
            }
            if out.len() >= max {
                break;
            }
        }
        out
    }

    /// Drops what can never be included any more: transactions whose nonce is
    /// spent, expired ones, and the explicitly `rejected`.
    pub fn prune(&mut self, state: &State, rejected: &[Hash]) {
        let rejected: HashSet<&Hash> = rejected.iter().collect();
        let height = state.height().saturating_add(1);
        let ids = &mut self.ids;
        self.by_sender.retain(|sender, queue| {
            let next = state.account(sender).map_or(0, |a| a.nonce);
            queue.retain(|nonce, tx| {
                let id = tx.id();
                let keep =
                    *nonce >= next && tx.body.expires_at >= height && !rejected.contains(&id);
                if !keep {
                    ids.remove(&id);
                }
                keep
            });
            !queue.is_empty()
        });
    }
}
