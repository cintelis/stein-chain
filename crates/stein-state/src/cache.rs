//! Node-local caches. Neither changes what is valid; both only avoid repeating
//! work. A cache miss always falls back to full verification.

use std::collections::HashMap;
use std::hash::Hash as StdHash;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use stein_crypto::{Hash, VerifyingKey, hash};
use stein_types::Transaction;

/// A map bounded to about `capacity` entries using two generations: when the
/// current generation fills, it becomes the previous one and the old previous
/// one is dropped. Recently used entries survive; memory never exceeds two
/// generations. O(1) everything, no LRU bookkeeping.
pub struct TwoGen<K, V> {
    current: HashMap<K, V>,
    previous: HashMap<K, V>,
    generation_size: usize,
}

impl<K: Eq + StdHash + Clone, V: Clone> TwoGen<K, V> {
    /// A cache holding at most `capacity` entries.
    pub fn new(capacity: usize) -> Self {
        Self {
            current: HashMap::new(),
            previous: HashMap::new(),
            generation_size: (capacity / 2).max(1),
        }
    }

    /// Looks `key` up, promoting a hit from the previous generation.
    pub fn get(&mut self, key: &K) -> Option<V> {
        if let Some(v) = self.current.get(key) {
            return Some(v.clone());
        }
        let v = self.previous.remove(key)?;
        self.insert(key.clone(), v.clone());
        Some(v)
    }

    /// Inserts, rotating generations when the current one is full.
    pub fn insert(&mut self, key: K, value: V) {
        if self.current.len() >= self.generation_size {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(key, value);
    }

    /// Removes `key` from both generations.
    pub fn remove(&mut self, key: &K) {
        self.current.remove(key);
        self.previous.remove(key);
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.current.len().saturating_add(self.previous.len())
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

const SIG_CACHE_CONTEXT: &str = "stein sigcache v1";

/// Remembers which (key, body, witness) triples have verified, so a signature
/// checked at mempool admission is not checked again when the block that
/// carries it is built or imported. Halves signature CPU in the common case.
///
/// The entry commits to the verifying key's fingerprint, the transaction id
/// and the full witness hash: a cached result can only ever vouch for exactly
/// the bytes that were verified. Entries are created only by a successful
/// verification on this node, so a peer cannot inject one.
pub struct SigCache {
    entries: Mutex<TwoGen<Hash, ()>>,
    hits: AtomicU64,
}

impl SigCache {
    /// A cache for about `capacity` signatures (32 bytes plus overhead each).
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(TwoGen::new(capacity)),
            hits: AtomicU64::new(0),
        }
    }

    /// The cache key for `tx` verified under `key`.
    pub fn entry(key: &VerifyingKey, tx: &Transaction) -> Hash {
        hash(
            SIG_CACHE_CONTEXT,
            &[&key.address().0, &tx.id().0, &tx.witness.hash().0],
        )
    }

    /// For each entry, whether it is cached. One lock for the whole batch.
    pub fn lookup(&self, entries: &[Hash]) -> Vec<bool> {
        let mut cache = self.entries.lock();
        let found: Vec<bool> = entries.iter().map(|e| cache.get(e).is_some()).collect();
        let hits = found.iter().filter(|f| **f).count() as u64;
        self.hits.fetch_add(hits, Ordering::Relaxed);
        found
    }

    /// Records entries that verified. One lock for the whole batch.
    pub fn record(&self, entries: impl IntoIterator<Item = Hash>) {
        let mut cache = self.entries.lock();
        for e in entries {
            cache.insert(e, ());
        }
    }

    /// Lookups answered from the cache since start.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_generations_bound_memory_and_keep_recent_entries() {
        let mut c = TwoGen::new(4);
        for i in 0..100u32 {
            c.insert(i, i);
        }
        assert!(c.len() <= 4);
        assert_eq!(c.get(&99), Some(99));
        assert_eq!(c.get(&0), None);
        c.remove(&99);
        assert_eq!(c.get(&99), None);
    }
}
