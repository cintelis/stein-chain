//! Consensus-critical chain parameters, fixed at genesis.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

use stein_crypto::Scheme;

use crate::Amount;

/// Parameters every node must agree on. Part of the genesis hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ChainParams {
    /// Target time between blocks.
    pub block_time_ms: u64,
    /// Most transactions in one block.
    pub max_block_txs: u32,
    /// Most bytes of transactions (bodies plus witnesses) in one block.
    pub max_block_bytes: u64,
    /// Minimum fee per encoded byte of a transaction, in base units. Fees are
    /// charged per byte because bytes, not CPU, are the scarce resource.
    pub min_fee_per_byte: Amount,
    /// Furthest ahead of the current height a transaction may set its expiry.
    /// Bounds how long a signed transaction stays replayable from a mempool.
    pub max_tx_ttl_blocks: u64,
    /// How many finalized blocks keep their witnesses (signatures and keys)
    /// before a non-archive node may prune them.
    pub witness_retention_blocks: u64,
    /// Signature schemes accepted for account keys. Adding a scheme (e.g.
    /// FN-DSA-512 once FIPS 206 is final) is a parameter change, not a format
    /// change (SPEC §3.4).
    pub tx_schemes: Vec<Scheme>,
    /// Signature schemes accepted for validator consensus keys.
    pub validator_schemes: Vec<Scheme>,
}

impl Default for ChainParams {
    /// Devnet defaults. See SPEC §5 for how they were chosen.
    fn default() -> Self {
        Self {
            block_time_ms: 1_000,
            max_block_txs: 4_000,
            max_block_bytes: 16 * 1024 * 1024,
            min_fee_per_byte: 10,
            max_tx_ttl_blocks: 600,
            // 6 hours at 1 s blocks; older witnesses go to archive storage.
            witness_retention_blocks: 21_600,
            tx_schemes: vec![Scheme::MlDsa44, Scheme::MlDsa65],
            validator_schemes: vec![Scheme::MlDsa44, Scheme::MlDsa65],
        }
    }
}
