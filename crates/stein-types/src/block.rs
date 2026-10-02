//! Blocks, split into a small executed part and a large prunable witness part.
//!
//! The header commits to both through separate Merkle roots. Validators must
//! hold and verify every witness *before* voting for a block (SPEC §4.3); only
//! after finality, and after `witness_retention_blocks`, may a node drop them.
//! The `tx_root` then still proves which bodies were executed.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use stein_crypto::{Address, Hash, PublicKey, Signature, hash, merkle_root};

use crate::{ChainId, Height, Transaction, TxBody};

const WITNESS_CONTEXT: &str = "stein witness v1";
const BLOCK_CONTEXT: &str = "stein block header v1";

/// The authorisation for one transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Witness {
    /// Present only on the sender's first transaction (key registration).
    pub public_key: Option<PublicKey>,
    /// ML-DSA-65 signature over the body.
    pub signature: Signature,
}

impl Witness {
    /// Leaf hash in the witness tree.
    pub fn hash(&self) -> Hash {
        hash(WITNESS_CONTEXT, &[&borsh::to_vec(self).unwrap_or_default()])
    }
}

/// What the proposer signs and what consensus agrees on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct BlockHeader {
    /// Format version ([`BlockHeader::VERSION`]). Bumped only by protocol
    /// upgrades, e.g. when sharding adds fields (SHARD-READY I-7).
    pub version: u8,
    /// Network.
    pub chain_id: ChainId,
    /// Height of this block (first block is 1).
    pub height: Height,
    /// Hash of the previous header.
    pub parent: Hash,
    /// Proposer's clock, milliseconds since the Unix epoch. Must increase.
    pub timestamp_ms: u64,
    /// Validator that proposed the block.
    pub proposer: Address,
    /// Number of transactions.
    pub tx_count: u32,
    /// Merkle root of transaction ids, in block order.
    pub tx_root: Hash,
    /// Merkle root of witness hashes, in block order.
    pub witness_root: Hash,
    /// Merkle root of account state after executing this block.
    pub state_root: Hash,
}

impl BlockHeader {
    /// The block format this software produces and accepts.
    pub const VERSION: u8 = 1;

    /// The block hash.
    pub fn hash(&self) -> Hash {
        hash(BLOCK_CONTEXT, &[&self.encode()])
    }

    /// Canonical encoding: what the proposer signs.
    pub fn encode(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }
}

/// A full block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Block {
    /// The header.
    pub header: BlockHeader,
    /// The proposer's ML-DSA-65 signature over the header.
    pub proposer_signature: Signature,
    /// Transaction bodies, in execution order.
    pub bodies: Vec<TxBody>,
    /// Witnesses, one per body; `None` once pruned.
    pub witnesses: Option<Vec<Witness>>,
}

impl Block {
    /// Root over the ids of `bodies`.
    pub fn tx_root(bodies: &[TxBody]) -> Hash {
        let ids: Vec<Hash> = bodies.iter().map(TxBody::id).collect();
        merkle_root(&ids)
    }

    /// Root over `witnesses`.
    pub fn witness_root(witnesses: &[Witness]) -> Hash {
        let leaves: Vec<Hash> = witnesses.iter().map(Witness::hash).collect();
        merkle_root(&leaves)
    }

    /// The block hash.
    pub fn hash(&self) -> Hash {
        self.header.hash()
    }

    /// Whether bodies and witnesses match the header's counts and roots. A
    /// pruned block checks only its bodies.
    pub fn roots_match(&self) -> bool {
        let count_ok = self.bodies.len() == self.header.tx_count as usize;
        let tx_ok = Self::tx_root(&self.bodies) == self.header.tx_root;
        let witness_ok = match &self.witnesses {
            None => true,
            Some(w) => {
                w.len() == self.bodies.len() && Self::witness_root(w) == self.header.witness_root
            }
        };
        count_ok && tx_ok && witness_ok
    }

    /// Reassembles the block's transactions. `None` if pruned.
    pub fn transactions(&self) -> Option<Vec<Transaction>> {
        let witnesses = self.witnesses.as_ref()?;
        Some(
            self.bodies
                .iter()
                .zip(witnesses)
                .map(|(body, witness)| Transaction {
                    body: body.clone(),
                    witness: witness.clone(),
                })
                .collect(),
        )
    }

    /// Drops the witnesses, keeping everything the header commits to except
    /// the signatures themselves.
    pub fn prune(&mut self) {
        self.witnesses = None;
    }
}
