//! What explorers and wallets see: JSON views of blocks and transactions, and
//! the live events a node streams on `GET /events`.

use serde::{Deserialize, Serialize};
use stein_crypto::{Address, Hash, Scheme};
use stein_types::{Action, Block, Transaction, TxBody, Witness, format_amount};

/// One transaction, flattened for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxView {
    /// Transaction id.
    pub id: Hash,
    /// Paying account.
    pub sender: Address,
    /// Recipient.
    pub to: Address,
    /// Amount in base units (string: u128 does not fit JSON numbers).
    pub amount: String,
    /// Amount in STEIN.
    pub amount_stein: String,
    /// Fee in STEIN.
    pub fee_stein: String,
    /// Sender nonce.
    pub nonce: u64,
    /// Last height at which it could be included.
    pub expires_at: u64,
    /// Signature scheme, while the witness is retained.
    pub scheme: Option<Scheme>,
    /// Whether this transaction registered the sender's key.
    pub registers_key: Option<bool>,
    /// Encoded size in bytes, while the witness is retained.
    pub size: Option<u64>,
    /// Block height, once included.
    pub height: Option<u64>,
    /// Position in the block, once included.
    pub index: Option<u32>,
}

impl TxView {
    /// A view of `body`, with what the witness tells if it is still held.
    pub fn of(body: &TxBody, witness: Option<&Witness>, at: Option<(u64, u32)>) -> Self {
        let Action::Transfer { to, amount } = &body.action;
        let size = witness.map(|w| {
            Transaction {
                body: body.clone(),
                witness: w.clone(),
            }
            .encoded_len()
        });
        Self {
            id: body.id(),
            sender: body.sender,
            to: *to,
            amount: amount.to_string(),
            amount_stein: format_amount(*amount),
            fee_stein: format_amount(body.fee),
            nonce: body.nonce,
            expires_at: body.expires_at,
            scheme: witness.map(|w| w.signature.scheme()),
            registers_key: witness.map(|w| w.public_key.is_some()),
            size,
            height: at.map(|(h, _)| h),
            index: at.map(|(_, i)| i),
        }
    }

    /// A view of a transaction that is not yet in a block.
    pub fn pending(tx: &Transaction) -> Self {
        Self::of(&tx.body, Some(&tx.witness), None)
    }
}

/// One block, flattened for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockView {
    /// Height.
    pub height: u64,
    /// Block hash.
    pub hash: Hash,
    /// Previous block's hash.
    pub parent: Hash,
    /// Proposer's timestamp, ms since the Unix epoch.
    pub timestamp_ms: u64,
    /// Proposing validator.
    pub proposer: Address,
    /// Number of transactions.
    pub tx_count: u32,
    /// State root after the block.
    pub state_root: Hash,
    /// Total encoded transaction bytes, while witnesses are retained.
    pub size: Option<u64>,
    /// Whether the witnesses are still held (SPEC §5.5).
    pub witnesses_available: bool,
    /// The transactions.
    pub txs: Vec<TxView>,
}

impl BlockView {
    /// A view of `block`.
    pub fn of(block: &Block) -> Self {
        let h = &block.header;
        let witnesses = block.witnesses.as_deref();
        let txs: Vec<TxView> = block
            .bodies
            .iter()
            .enumerate()
            .map(|(i, body)| {
                let witness = witnesses.and_then(|w| w.get(i));
                TxView::of(body, witness, Some((h.height, i as u32)))
            })
            .collect();
        let size = witnesses.map(|_| txs.iter().filter_map(|t| t.size).sum());
        Self {
            height: h.height,
            hash: block.hash(),
            parent: h.parent,
            timestamp_ms: h.timestamp_ms,
            proposer: h.proposer,
            tx_count: h.tx_count,
            state_root: h.state_root,
            size,
            witnesses_available: witnesses.is_some(),
            txs,
        }
    }
}

/// A live event on `GET /events` (server-sent events, JSON data).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A block was produced (validator) or imported and verified (follower).
    Block(BlockView),
    /// A transaction was accepted into this node's mempool or forwarded.
    Pending(TxView),
}
