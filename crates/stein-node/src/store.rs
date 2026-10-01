//! Block storage: an append-only, fsynced log on disk plus an in-memory index.
//!
//! The log keeps full blocks (witnesses included) so a restarted node can
//! re-verify its own history. Memory keeps witnesses only for the retention
//! window. Production nodes replace this with RocksDB column families split
//! into bodies and witnesses (SPEC §5.5).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

use stein_crypto::{Address, Hash};
use stein_types::{Action, Block, Height};

/// Where each transaction landed.
#[derive(Debug, Clone, Copy)]
pub struct TxLocation {
    /// Block height.
    pub height: Height,
    /// Position in the block.
    pub index: u32,
}

/// Blocks by height, plus the log they are persisted to.
pub struct Store {
    log: File,
    blocks: Vec<Block>,
    tx_index: HashMap<Hash, TxLocation>,
    /// Transactions touching each address (as sender or recipient), oldest
    /// first. For explorers; production moves this to an indexer (SPEC §11.1).
    by_address: HashMap<Address, Vec<TxLocation>>,
    retention: u64,
    /// Transactions in all blocks held.
    tx_total: u64,
}

/// Most bytes one log record may claim: a corrupted length must not make the
/// node allocate gigabytes.
const MAX_RECORD: u32 = 256 * 1024 * 1024;

impl Store {
    /// Opens (or creates) the log at `path` and reads every block in it.
    /// The caller re-verifies them by importing them into state.
    pub fn open(path: &Path, retention: u64) -> std::io::Result<(Self, Vec<Block>)> {
        let mut recovered = Vec::new();
        let mut valid_len = 0u64;
        if path.exists() {
            let mut reader = BufReader::new(File::open(path)?);
            loop {
                let mut len = [0u8; 4];
                match reader.read_exact(&mut len) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e),
                }
                let len = u32::from_le_bytes(len);
                if len > MAX_RECORD {
                    break;
                }
                let mut bytes = vec![0u8; len as usize];
                if reader.read_exact(&mut bytes).is_err() {
                    break; // torn write at the tail: drop it
                }
                match borsh::from_slice::<Block>(&bytes) {
                    Ok(block) => recovered.push(block),
                    Err(_) => break,
                }
                valid_len = valid_len.saturating_add(4).saturating_add(u64::from(len));
            }
        }
        // Not opened in append mode: on Windows an append-only handle cannot
        // truncate. Cut any torn tail, then write from the end.
        let mut log = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        log.set_len(valid_len)?;
        log.seek(SeekFrom::End(0))?;
        Ok((
            Self {
                log,
                blocks: Vec::new(),
                tx_index: HashMap::new(),
                by_address: HashMap::new(),
                tx_total: 0,
                retention,
            },
            recovered,
        ))
    }

    /// Persists `block` (fsync) and indexes it. Call only after the block has
    /// been fully validated and applied.
    pub fn append(&mut self, block: Block, persist: bool) -> std::io::Result<()> {
        if persist {
            let bytes = borsh::to_vec(&block)?;
            let len = u32::try_from(bytes.len()).map_err(|_| ErrorKind::InvalidData)?;
            self.log.write_all(&len.to_le_bytes())?;
            self.log.write_all(&bytes)?;
            self.log.sync_data()?;
        }
        let height = block.header.height;
        for (index, body) in block.bodies.iter().enumerate() {
            let at = TxLocation {
                height,
                index: index as u32,
            };
            self.tx_index.insert(body.id(), at);
            self.tx_total = self.tx_total.saturating_add(1);
            let Action::Transfer { to, .. } = &body.action;
            self.by_address.entry(body.sender).or_default().push(at);
            if *to != body.sender {
                self.by_address.entry(*to).or_default().push(at);
            }
        }
        self.blocks.push(block);
        self.prune_witnesses();
        Ok(())
    }

    fn prune_witnesses(&mut self) {
        let Some(tip) = self.blocks.last().map(|b| b.header.height) else {
            return;
        };
        let Some(cutoff) = tip.checked_sub(self.retention) else {
            return;
        };
        if let Some(block) = self.blocks.get_mut(cutoff.saturating_sub(1) as usize) {
            block.prune();
        }
    }

    /// The block at `height` (1-based).
    pub fn block(&self, height: Height) -> Option<&Block> {
        self.blocks.get(height.checked_sub(1)? as usize)
    }

    /// Up to `max` consecutive blocks from `from`, capped at about `max_bytes`
    /// of encoding (always at least one block if `from` exists).
    pub fn range(&self, from: Height, max: u64, max_bytes: usize) -> Vec<&Block> {
        let mut out = Vec::new();
        let mut bytes = 0usize;
        let mut height = from;
        while (out.len() as u64) < max {
            let Some(block) = self.block(height) else {
                break;
            };
            let size = borsh::object_length(block).unwrap_or(usize::MAX);
            if !out.is_empty() && bytes.saturating_add(size) > max_bytes {
                break;
            }
            bytes = bytes.saturating_add(size);
            out.push(block);
            height = height.saturating_add(1);
        }
        out
    }

    /// Transactions in all blocks held.
    pub fn tx_total(&self) -> u64 {
        self.tx_total
    }

    /// Height of the newest block held.
    pub fn tip(&self) -> Height {
        self.blocks.last().map_or(0, |b| b.header.height)
    }

    /// Up to `limit` locations of transactions touching `address`, newest first.
    pub fn address_txs(&self, address: &Address, limit: usize) -> Vec<TxLocation> {
        self.by_address
            .get(address)
            .map(|v| v.iter().rev().take(limit).copied().collect())
            .unwrap_or_default()
    }

    /// Where a transaction was included.
    pub fn locate(&self, id: &Hash) -> Option<TxLocation> {
        self.tx_index.get(id).copied()
    }
}
