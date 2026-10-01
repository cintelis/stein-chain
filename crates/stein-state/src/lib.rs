//! The Stein Chain state machine.
//!
//! Two entry points change state, and both are all-or-nothing:
//!
//! * [`State::build_block`] — the proposer's path. Takes candidate
//!   transactions, drops the invalid ones, executes the rest and signs a block.
//! * [`State::import_block`] — every other validator's path. Re-checks
//!   *everything*: proposer, signature over the header, every transaction
//!   signature, every balance change, and the resulting state root. A single
//!   bad transaction rejects the whole block. Nothing a proposer claims is
//!   taken on trust (SPEC §4.3).

mod cache;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub use cache::{SigCache, TwoGen};
use parking_lot::Mutex;

use borsh::{BorshDeserialize, BorshSerialize};
use rayon::prelude::*;
use serde::Serialize;
use stein_crypto::{
    Address, Domain, Hash, KeyError, PublicKey, SecretKey, VerifyingKey, hash, merkle_root,
};
use stein_types::{
    Action, Amount, Block, BlockHeader, ChainId, ChainParams, Genesis, GenesisError, Height,
    Transaction, TxBody, TxError, Witness,
};

const ACCOUNT_LEAF: &str = "stein account leaf v1";

/// One account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, BorshSerialize, BorshDeserialize)]
pub struct Account {
    /// Spendable base units.
    pub balance: Amount,
    /// Next nonce the account must use.
    pub nonce: u64,
    /// Registered key (any accepted scheme); set by the account's first transaction.
    pub public_key: Option<PublicKey>,
}

/// Why a block was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlockError {
    /// Wrong network.
    #[error("wrong chain id")]
    WrongChain,
    /// Not the next height.
    #[error("expected height {expected}, got {got}")]
    WrongHeight {
        /// Next height.
        expected: Height,
        /// Block's height.
        got: Height,
    },
    /// Does not extend the current head.
    #[error("parent is not the current head")]
    WrongParent,
    /// Timestamp not after the parent's.
    #[error("timestamp must increase")]
    Timestamp,
    /// Proposer is not the validator scheduled for this height.
    #[error("proposer is not scheduled for this height")]
    WrongProposer,
    /// Proposer signature does not verify.
    #[error("invalid proposer signature")]
    BadProposerSignature,
    /// Witnesses missing: a validator never votes on what it cannot verify.
    #[error("block arrived without witnesses")]
    MissingWitnesses,
    /// Bodies or witnesses do not match the header.
    #[error("bodies or witnesses do not match the header")]
    RootMismatch,
    /// Block exceeds count or byte limits.
    #[error("block exceeds limits")]
    TooLarge,
    /// A transaction is invalid.
    #[error("transaction {index} invalid: {error}")]
    InvalidTx {
        /// Position in the block.
        index: usize,
        /// Why.
        error: TxError,
    },
    /// Executing the block does not give the claimed state root.
    #[error("state root mismatch")]
    StateRoot,
}

/// A validator in the active set.
#[derive(Debug, Clone)]
pub struct Validator {
    /// Parsed consensus key.
    pub key: VerifyingKey,
    /// Voting power.
    pub power: u64,
}

/// Account state plus the chain head.
pub struct State {
    chain_id: ChainId,
    params: ChainParams,
    accounts: BTreeMap<Address, Account>,
    /// Parsed keys of recently active accounts: parse once, verify many
    /// times. Bounded, so memory does not grow with the number of accounts.
    key_cache: Mutex<TwoGen<Address, VerifyingKey>>,
    /// Signatures already verified on this node (SPEC §5.3).
    sig_cache: Arc<SigCache>,
    validators: Vec<Validator>,
    height: Height,
    head: Hash,
    head_timestamp_ms: u64,
    state_root: Hash,
}

/// Changes made while executing a block, not yet committed.
type Overlay = BTreeMap<Address, Account>;

/// Parsed keys kept in memory.
const KEY_CACHE_CAPACITY: usize = 200_000;
/// Verified signatures remembered (about 50 blocks' worth at 4,000 tx/block).
const SIG_CACHE_CAPACITY: usize = 200_000;

impl State {
    /// The state described by `genesis`.
    pub fn from_genesis(genesis: &Genesis) -> Result<Self, GenesisError> {
        genesis.validate()?;
        let accounts: BTreeMap<Address, Account> = genesis
            .allocations
            .iter()
            .map(|a| {
                (
                    a.address,
                    Account {
                        balance: a.amount,
                        ..Account::default()
                    },
                )
            })
            .collect();
        let mut validators: Vec<Validator> = genesis
            .validators
            .iter()
            .map(|v| {
                VerifyingKey::new(&v.public_key)
                    .map(|key| Validator {
                        key,
                        power: v.power,
                    })
                    .map_err(|_| GenesisError::BadValidator)
            })
            .collect::<Result<_, _>>()?;
        validators.sort_by_key(|v| v.key.address());
        let mut state = Self {
            chain_id: genesis.chain_id,
            params: genesis.params.clone(),
            accounts,
            key_cache: Mutex::new(TwoGen::new(KEY_CACHE_CAPACITY)),
            sig_cache: Arc::new(SigCache::new(SIG_CACHE_CAPACITY)),
            validators,
            height: 0,
            head: genesis.hash(),
            head_timestamp_ms: genesis.genesis_time_ms,
            state_root: Hash::ZERO,
        };
        state.state_root = state.compute_root();
        Ok(state)
    }

    /// Network id.
    pub fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    /// Consensus parameters.
    pub fn params(&self) -> &ChainParams {
        &self.params
    }

    /// Height of the last executed block (0 = genesis).
    pub fn height(&self) -> Height {
        self.height
    }

    /// Hash of the last executed block (the genesis hash at height 0).
    pub fn head(&self) -> Hash {
        self.head
    }

    /// Current state root.
    pub fn state_root(&self) -> Hash {
        self.state_root
    }

    /// An account, if it exists.
    pub fn account(&self, address: &Address) -> Option<&Account> {
        self.accounts.get(address)
    }

    /// The active validator set, ordered by address.
    pub fn validators(&self) -> &[Validator] {
        &self.validators
    }

    /// The validator scheduled to propose at `height`: round-robin over the
    /// set. Stake-weighted selection with BFT rounds replaces this when
    /// consensus lands (SPEC §6).
    pub fn proposer_for(&self, height: Height) -> &Validator {
        // Genesis validation guarantees a non-empty set.
        let n = self.validators.len() as u64;
        &self.validators[height.checked_rem(n).unwrap_or(0) as usize]
    }

    /// The signature cache, for metrics.
    pub fn sig_cache(&self) -> &SigCache {
        &self.sig_cache
    }

    fn registered_key(&self, address: &Address) -> Option<VerifyingKey> {
        if let Some(key) = self.key_cache.lock().get(address) {
            return Some(key);
        }
        let key = VerifyingKey::new(self.accounts.get(address)?.public_key.as_ref()?).ok()?;
        self.key_cache.lock().insert(*address, key.clone());
        Some(key)
    }

    /// Verifies each transaction's signature under its resolved key, in
    /// parallel, skipping those this node has already verified.
    fn verify_signatures(&self, items: &[(&Transaction, &VerifyingKey)]) -> Vec<bool> {
        let entries: Vec<Hash> = items
            .par_iter()
            .map(|(tx, key)| SigCache::entry(key, tx))
            .collect();
        let cached = self.sig_cache.lookup(&entries);
        let valid: Vec<bool> = items
            .par_iter()
            .zip(&cached)
            .map(|((tx, key), hit)| {
                *hit || key
                    .verify(Domain::Transaction, &tx.body.encode(), tx.signature())
                    .is_ok()
            })
            .collect();
        self.sig_cache.record(
            entries
                .into_iter()
                .zip(cached.iter().zip(&valid))
                .filter(|(_, (hit, ok))| !**hit && **ok)
                .map(|(e, _)| e),
        );
        valid
    }

    /// Finds the key that must have signed `tx`. `pending` holds keys
    /// registered by earlier transactions in the same block.
    fn resolve_key(
        &self,
        tx: &Transaction,
        pending: &HashMap<Address, VerifyingKey>,
    ) -> Result<VerifyingKey, TxError> {
        let sender = tx.body.sender;
        let registered = pending
            .get(&sender)
            .cloned()
            .or_else(|| self.registered_key(&sender));
        match (tx.public_key(), registered) {
            (Some(_), Some(_)) => Err(TxError::KeyAlreadyRegistered),
            (Some(key), None) => VerifyingKey::new(key).map_err(|_| TxError::BadKey),
            (None, Some(key)) => Ok(key),
            (None, None) => Err(TxError::KeyMissing),
        }
    }

    /// Full validation of one transaction against current state, signature
    /// included. For mempool admission. `queued_key` is the key carried by
    /// the sender's queued, not yet included, first transaction: it lets a
    /// wallet send several transactions before the first lands in a block.
    pub fn check_transaction(
        &self,
        tx: &Transaction,
        queued_key: Option<&PublicKey>,
    ) -> Result<(), TxError> {
        tx.check_stateless(self.chain_id, &self.params, self.height.saturating_add(1))?;
        let mut pending = HashMap::new();
        if let Some(key) = queued_key
            && tx.public_key().is_none()
            && key.address() == tx.body.sender
            && self.registered_key(&tx.body.sender).is_none()
        {
            pending.insert(
                tx.body.sender,
                VerifyingKey::new(key).map_err(|_| TxError::BadKey)?,
            );
        }
        let key = self.resolve_key(tx, &pending)?;
        let account = self
            .accounts
            .get(&tx.body.sender)
            .cloned()
            .unwrap_or_default();
        if tx.body.nonce < account.nonce {
            return Err(TxError::BadNonce {
                expected: account.nonce,
                got: tx.body.nonce,
            });
        }
        match self.verify_signatures(&[(tx, &key)]).as_slice() {
            [true] => Ok(()),
            _ => Err(TxError::BadSignature),
        }
    }

    /// Executes `body` on top of `overlay` (falling back to committed state).
    /// Atomic: on error, `overlay` is untouched.
    fn apply(
        &self,
        overlay: &mut Overlay,
        body: &TxBody,
        attached_key: Option<&PublicKey>,
        proposer: Address,
    ) -> Result<(), TxError> {
        // Changes are staged in `local` and merged only on success. Sender,
        // recipient and proposer may be the same account; staging through one
        // map keeps that correct.
        let mut local = Overlay::new();
        let touch = |local: &mut Overlay, address: Address| -> Account {
            local
                .remove(&address)
                .or_else(|| overlay.get(&address).cloned())
                .or_else(|| self.accounts.get(&address).cloned())
                .unwrap_or_default()
        };

        let mut sender = touch(&mut local, body.sender);
        if body.nonce != sender.nonce {
            return Err(TxError::BadNonce {
                expected: sender.nonce,
                got: body.nonce,
            });
        }
        match (attached_key, &sender.public_key) {
            (Some(_), Some(_)) => return Err(TxError::KeyAlreadyRegistered),
            (None, None) => return Err(TxError::KeyMissing),
            (Some(key), None) => sender.public_key = Some(key.clone()),
            (None, Some(_)) => {}
        }

        let Action::Transfer { to, amount } = &body.action;
        let debit = amount.checked_add(body.fee).ok_or(TxError::Overflow)?;
        sender.balance = sender
            .balance
            .checked_sub(debit)
            .ok_or(TxError::InsufficientBalance)?;
        sender.nonce = sender.nonce.checked_add(1).ok_or(TxError::Overflow)?;
        local.insert(body.sender, sender);

        // Credits come after the debit is staged, so a transfer to oneself is
        // just a fee payment.
        for (address, credit) in [(*to, *amount), (proposer, body.fee)] {
            let mut account = touch(&mut local, address);
            account.balance = account
                .balance
                .checked_add(credit)
                .ok_or(TxError::Overflow)?;
            local.insert(address, account);
        }
        overlay.extend(local);
        Ok(())
    }

    /// Commits `overlay`, returning the previous values so the commit can be
    /// undone.
    fn commit(&mut self, overlay: Overlay) -> Vec<(Address, Option<Account>)> {
        overlay
            .into_iter()
            .map(|(address, account)| (address, self.accounts.insert(address, account)))
            .collect()
    }

    fn undo(&mut self, undo: Vec<(Address, Option<Account>)>) {
        let mut key_cache = self.key_cache.lock();
        for (address, previous) in undo {
            key_cache.remove(&address);
            match previous {
                Some(account) => self.accounts.insert(address, account),
                None => self.accounts.remove(&address),
            };
        }
    }

    /// Merkle root over all accounts in address order. O(n) per block; the
    /// production design replaces it with an incrementally updated Jellyfish
    /// Merkle Tree (SPEC §5.4).
    fn compute_root(&self) -> Hash {
        let leaves: Vec<Hash> = self
            .accounts
            .par_iter()
            .map(|(address, account)| {
                hash(
                    ACCOUNT_LEAF,
                    &[&address.0, &borsh::to_vec(account).unwrap_or_default()],
                )
            })
            .collect();
        merkle_root(&leaves)
    }

    /// Proposer path: builds, executes, commits and signs the next block from
    /// `candidates`. Returns the block and the candidates that were left out,
    /// with the reason.
    pub fn build_block(
        &mut self,
        candidates: Vec<Transaction>,
        proposer: &SecretKey,
        timestamp_ms: u64,
    ) -> Result<(Block, Vec<(Hash, TxError)>), KeyError> {
        let height = self.height.saturating_add(1);
        let mut rejected = Vec::new();

        // 1. Cheap checks and key resolution, sequential (keys registered
        //    earlier in the block must be visible to later transactions).
        let mut pending: HashMap<Address, VerifyingKey> = HashMap::new();
        let mut staged: Vec<(Transaction, VerifyingKey)> = Vec::new();
        let (mut count, mut bytes) = (0u32, 0u64);
        for tx in candidates {
            if count >= self.params.max_block_txs {
                break;
            }
            let size = tx.encoded_len();
            if bytes.saturating_add(size) > self.params.max_block_bytes {
                continue;
            }
            let checked = tx
                .check_stateless(self.chain_id, &self.params, height)
                .and_then(|()| self.resolve_key(&tx, &pending));
            match checked {
                Ok(key) => {
                    if tx.public_key().is_some() {
                        pending.insert(tx.body.sender, key.clone());
                    }
                    count = count.saturating_add(1);
                    bytes = bytes.saturating_add(size);
                    staged.push((tx, key));
                }
                Err(e) => rejected.push((tx.id(), e)),
            }
        }

        // 2. Signatures, in parallel across all cores; most were verified at
        //    mempool admission and come straight from the cache.
        let items: Vec<(&Transaction, &VerifyingKey)> =
            staged.iter().map(|(tx, key)| (tx, key)).collect();
        let valid = self.verify_signatures(&items);

        // 3. Execution, sequential. A transaction that fails is left out.
        let proposer_address = proposer.address();
        let mut overlay = Overlay::new();
        let mut bodies = Vec::new();
        let mut witnesses: Vec<Witness> = Vec::new();
        for ((tx, _), ok) in staged.into_iter().zip(valid) {
            if !ok {
                rejected.push((tx.id(), TxError::BadSignature));
                continue;
            }
            match self.apply(&mut overlay, &tx.body, tx.public_key(), proposer_address) {
                Ok(()) => {
                    bodies.push(tx.body);
                    witnesses.push(tx.witness);
                }
                Err(e) => rejected.push((tx.id(), e)),
            }
        }

        self.commit(overlay);
        let state_root = self.compute_root();
        let header = BlockHeader {
            chain_id: self.chain_id,
            height,
            parent: self.head,
            timestamp_ms: timestamp_ms.max(self.head_timestamp_ms.saturating_add(1)),
            proposer: proposer_address,
            tx_count: bodies.len() as u32,
            tx_root: Block::tx_root(&bodies),
            witness_root: Block::witness_root(&witnesses),
            state_root,
        };
        let proposer_signature = proposer.sign(Domain::Proposal, &header.encode())?;
        self.advance(&header, state_root);
        Ok((
            Block {
                header,
                proposer_signature,
                bodies,
                witnesses: Some(witnesses),
            },
            rejected,
        ))
    }

    fn advance(&mut self, header: &BlockHeader, state_root: Hash) {
        self.height = header.height;
        self.head = header.hash();
        self.head_timestamp_ms = header.timestamp_ms;
        self.state_root = state_root;
    }

    /// Validator path: verifies `block` completely and, only if every check
    /// passes, applies it.
    pub fn import_block(&mut self, block: &Block) -> Result<(), BlockError> {
        let h = &block.header;
        let expected = self.height.saturating_add(1);
        if h.chain_id != self.chain_id {
            return Err(BlockError::WrongChain);
        }
        if h.height != expected {
            return Err(BlockError::WrongHeight {
                expected,
                got: h.height,
            });
        }
        if h.parent != self.head {
            return Err(BlockError::WrongParent);
        }
        if h.timestamp_ms <= self.head_timestamp_ms {
            return Err(BlockError::Timestamp);
        }
        let proposer = self.proposer_for(h.height);
        if proposer.key.address() != h.proposer {
            return Err(BlockError::WrongProposer);
        }
        proposer
            .key
            .verify(Domain::Proposal, &h.encode(), &block.proposer_signature)
            .map_err(|_| BlockError::BadProposerSignature)?;
        let txs = block.transactions().ok_or(BlockError::MissingWitnesses)?;
        if !block.roots_match() {
            return Err(BlockError::RootMismatch);
        }
        let bytes: u64 = txs.iter().map(Transaction::encoded_len).sum();
        if txs.len() > self.params.max_block_txs as usize || bytes > self.params.max_block_bytes {
            return Err(BlockError::TooLarge);
        }

        let invalid = |index: usize, error: TxError| BlockError::InvalidTx { index, error };
        let mut pending = HashMap::new();
        let mut keys = Vec::with_capacity(txs.len());
        for (i, tx) in txs.iter().enumerate() {
            tx.check_stateless(self.chain_id, &self.params, h.height)
                .map_err(|e| invalid(i, e))?;
            let key = self.resolve_key(tx, &pending).map_err(|e| invalid(i, e))?;
            if tx.public_key().is_some() {
                pending.insert(tx.body.sender, key.clone());
            }
            keys.push(key);
        }

        let items: Vec<(&Transaction, &VerifyingKey)> = txs.iter().zip(&keys).collect();
        if let Some(i) = self.verify_signatures(&items).iter().position(|ok| !ok) {
            return Err(invalid(i, TxError::BadSignature));
        }

        let mut overlay = Overlay::new();
        for (i, tx) in txs.iter().enumerate() {
            self.apply(&mut overlay, &tx.body, tx.public_key(), h.proposer)
                .map_err(|e| invalid(i, e))?;
        }
        let undo = self.commit(overlay);
        let root = self.compute_root();
        if root != h.state_root {
            self.undo(undo);
            return Err(BlockError::StateRoot);
        }
        self.advance(h, root);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
