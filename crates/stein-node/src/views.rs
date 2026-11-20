//! What explorers and wallets see: JSON views of blocks and transactions, and
//! the live events a node streams on `GET /events`.

use serde::{Deserialize, Serialize};
use stein_crypto::{Address, Hash, Scheme};
use stein_types::{Block, Outcome, Transaction, TxBody, Witness, format_amount};

fn transfer() -> String {
    "transfer".into()
}

/// One transaction, flattened for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxView {
    /// Transaction id.
    pub id: Hash,
    /// Paying account.
    pub sender: Address,
    /// What it does: `transfer`, `issue_token`, `transfer_token`,
    /// `add_liquidity`, `remove_liquidity` or `swap`.
    #[serde(default = "transfer")]
    pub kind: String,
    /// The other account it is about: the recipient, the token it creates,
    /// or the pool.
    pub to: Address,
    /// STEIN the sender sends, in base units (string: u128 does not fit JSON
    /// numbers). Zero when only tokens move.
    pub amount: String,
    /// The same in STEIN.
    pub amount_stein: String,
    /// The action in full, as in the block.
    #[serde(default)]
    pub action: serde_json::Value,
    /// What it did, once included, when that is more than the action says:
    /// the token created, what a pool paid, or why a pool returned what it
    /// was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// Fee in base units (string).
    pub fee: String,
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
    /// A view of `body`, with what the witness tells if it is still held, and
    /// what executing it did if it is in a block.
    pub fn of(
        body: &TxBody,
        witness: Option<&Witness>,
        at: Option<(u64, u32)>,
        outcome: Option<&Outcome>,
    ) -> Self {
        let amount = body.action.stein_debit();
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
            kind: body.action.kind().into(),
            to: body.action.counterparty(&body.sender, body.nonce),
            amount: amount.to_string(),
            amount_stein: format_amount(amount),
            action: serde_json::to_value(&body.action).unwrap_or_default(),
            outcome: outcome.filter(|o| **o != Outcome::Applied).cloned(),
            fee: body.fee.to_string(),
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
        Self::of(&tx.body, Some(&tx.witness), None, None)
    }
}

/// One block, flattened for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockView {
    /// Shard. Always 0 until sharding (SHARD-READY I-8).
    pub shard_id: u16,
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
    /// Fees paid in the block, in STEIN (to the proposer on the devnet).
    pub fees_stein: String,
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
    /// A view of `block`. `outcomes` are what its transactions did, in block
    /// order (see `Store::outcomes_for`).
    pub fn of(block: &Block, outcomes: &[Outcome]) -> Self {
        let h = &block.header;
        let witnesses = block.witnesses.as_deref();
        let txs: Vec<TxView> = block
            .bodies
            .iter()
            .enumerate()
            .map(|(i, body)| {
                let witness = witnesses.and_then(|w| w.get(i));
                TxView::of(body, witness, Some((h.height, i as u32)), outcomes.get(i))
            })
            .collect();
        let size = witnesses.map(|_| txs.iter().filter_map(|t| t.size).sum());
        let fees = block
            .bodies
            .iter()
            .fold(0u128, |sum, b| sum.saturating_add(b.fee));
        Self {
            shard_id: 0,
            height: h.height,
            hash: block.hash(),
            parent: h.parent,
            timestamp_ms: h.timestamp_ms,
            proposer: h.proposer,
            tx_count: h.tx_count,
            fees_stein: format_amount(fees),
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
