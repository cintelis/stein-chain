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

use std::collections::{BTreeMap, HashMap, HashSet};
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
    Action, Amount, Asset, Block, BlockHeader, ChainId, ChainParams, Genesis, GenesisError, Height,
    MIN_LIQUIDITY, Outcome, Pool, PoolError, Token, Transaction, TxBody, TxError, Witness, deposit,
    first_shares, pool_address, swap_output, withdrawal,
};

const ACCOUNT_LEAF: &str = "stein account leaf v1";
const TOKEN_LEAF: &str = "stein token leaf v1";
const HOLDING_LEAF: &str = "stein holding leaf v1";
const POOL_LEAF: &str = "stein pool leaf v1";
const STATE_ROOT: &str = "stein state root v2";

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
    /// A block format this node does not support.
    #[error("unsupported block version {0}")]
    UnsupportedVersion(u8),
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
    /// Tokens, by address (SPEC §4.6).
    tokens: BTreeMap<Address, Token>,
    /// Token and pool-share balances, by (holder, asset address): each
    /// holder's are contiguous, under its own address (SHARD-READY I-4). A
    /// holder of none has no entry.
    holdings: BTreeMap<(Address, Address), Amount>,
    /// Liquidity pools, by address (SPEC §4.7).
    pools: BTreeMap<Address, Pool>,
    /// What each transaction of the last block executed did.
    last_outcomes: Vec<Outcome>,
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
#[derive(Default, Clone)]
struct Overlay {
    accounts: BTreeMap<Address, Account>,
    /// Zero means the holder has none left: the entry goes at commit.
    holdings: BTreeMap<(Address, Address), Amount>,
    tokens: BTreeMap<Address, Token>,
    pools: BTreeMap<Address, Pool>,
}

impl Overlay {
    fn extend(&mut self, newer: Self) {
        self.accounts.extend(newer.accounts);
        self.holdings.extend(newer.holdings);
        self.tokens.extend(newer.tokens);
        self.pools.extend(newer.pools);
    }
}

/// What a commit replaced, so that it can be undone.
struct Undo {
    accounts: Vec<(Address, Option<Account>)>,
    holdings: Vec<((Address, Address), Option<Amount>)>,
    tokens: Vec<(Address, Option<Token>)>,
    pools: Vec<(Address, Option<Pool>)>,
}

fn restore<K: Ord, V>(map: &mut BTreeMap<K, V>, undo: Vec<(K, Option<V>)>) {
    for (key, previous) in undo {
        match previous {
            Some(value) => map.insert(key, value),
            None => map.remove(&key),
        };
    }
}

/// What one transaction sees while it executes: its own changes, over the
/// block's so far, over committed state. Its changes join the block's only if
/// it succeeds.
struct Stage<'a> {
    state: &'a State,
    block: &'a Overlay,
    own: Overlay,
}

impl Stage<'_> {
    fn account(&self, address: &Address) -> Account {
        self.own
            .accounts
            .get(address)
            .or_else(|| self.block.accounts.get(address))
            .or_else(|| self.state.accounts.get(address))
            .cloned()
            .unwrap_or_default()
    }

    fn pool(&self, address: &Address) -> Option<Pool> {
        self.own
            .pools
            .get(address)
            .or_else(|| self.block.pools.get(address))
            .or_else(|| self.state.pools.get(address))
            .cloned()
    }

    fn balance(&self, holder: &Address, asset: &Asset) -> Amount {
        match asset {
            Asset::Stein => self.account(holder).balance,
            Asset::Token(token) => {
                let key = (*holder, *token);
                self.own
                    .holdings
                    .get(&key)
                    .or_else(|| self.block.holdings.get(&key))
                    .or_else(|| self.state.holdings.get(&key))
                    .copied()
                    .unwrap_or(0)
            }
        }
    }

    fn set_balance(&mut self, holder: &Address, asset: &Asset, amount: Amount) {
        match asset {
            Asset::Stein => {
                let mut account = self.account(holder);
                account.balance = amount;
                self.own.accounts.insert(*holder, account);
            }
            Asset::Token(token) => {
                self.own.holdings.insert((*holder, *token), amount);
            }
        }
    }

    fn debit(&mut self, holder: &Address, asset: &Asset, amount: Amount) -> Result<(), TxError> {
        let left = self
            .balance(holder, asset)
            .checked_sub(amount)
            .ok_or(TxError::InsufficientBalance)?;
        self.set_balance(holder, asset, left);
        Ok(())
    }

    /// Credits have no precondition. The overflow check is a defensive
    /// assertion: every supply is below 2^127 (SHARD-READY I-1).
    fn credit(&mut self, holder: &Address, asset: &Asset, amount: Amount) -> Result<(), TxError> {
        let total = self
            .balance(holder, asset)
            .checked_add(amount)
            .ok_or(TxError::Overflow)?;
        self.set_balance(holder, asset, total);
        Ok(())
    }
}

/// The pool's half of a pool operation, run after the sender's half has
/// debited what the transaction sends (SPEC §4.7). It reads the pool's state,
/// which is not the sender's, so its result cannot be a condition of the
/// transaction's validity (SHARD-READY I-2): an error here leaves the
/// transaction valid and fee-paying, and the caller returns what was sent.
fn pool_step(stage: &mut Stage<'_>, body: &TxBody) -> Result<Outcome, PoolError> {
    let sender = &body.sender;
    let fits = |moved: Result<(), TxError>| moved.map_err(|_| PoolError::OutOfRange);
    match &body.action {
        Action::AddLiquidity {
            asset_a,
            amount_a,
            asset_b,
            amount_b,
            min_shares,
        } => {
            let address = pool_address(*asset_a, *asset_b);
            let share = Asset::Token(address);
            let existing = stage.pool(&address).map_or(0, |pool| pool.shares);
            let (shares, used_a, used_b, total) = if existing == 0 {
                // The first deposit sets the price. The pool keeps
                // MIN_LIQUIDITY shares for itself, for good.
                let minted = first_shares(*amount_a, *amount_b);
                let shares = minted
                    .checked_sub(MIN_LIQUIDITY)
                    .filter(|shares| *shares > 0)
                    .ok_or(PoolError::TooLittleLiquidity)?;
                fits(stage.credit(&address, &share, MIN_LIQUIDITY))?;
                (shares, *amount_a, *amount_b, minted)
            } else {
                let d = deposit(
                    stage.balance(&address, asset_a),
                    stage.balance(&address, asset_b),
                    existing,
                    *amount_a,
                    *amount_b,
                )
                .ok_or(PoolError::OutOfRange)?;
                if d.shares == 0 {
                    return Err(PoolError::Nothing);
                }
                let total = existing
                    .checked_add(d.shares)
                    .ok_or(PoolError::OutOfRange)?;
                (d.shares, d.used_a, d.used_b, total)
            };
            if shares < *min_shares {
                return Err(PoolError::BelowMinimum);
            }
            for (asset, offered, used) in [(asset_a, amount_a, used_a), (asset_b, amount_b, used_b)]
            {
                let unused = offered.checked_sub(used).ok_or(PoolError::OutOfRange)?;
                fits(stage.credit(&address, asset, used))?;
                fits(stage.credit(sender, asset, unused))?;
            }
            fits(stage.credit(sender, &share, shares))?;
            stage.own.pools.insert(
                address,
                Pool {
                    asset_a: *asset_a,
                    asset_b: *asset_b,
                    shares: total,
                },
            );
            Ok(Outcome::LiquidityAdded {
                pool: address,
                shares,
                used_a,
                used_b,
            })
        }
        Action::RemoveLiquidity {
            pool: address,
            shares,
            min_a,
            min_b,
        } => {
            let mut pool = stage.pool(address).ok_or(PoolError::NoPool)?;
            let (out_a, out_b) = withdrawal(
                stage.balance(address, &pool.asset_a),
                stage.balance(address, &pool.asset_b),
                pool.shares,
                *shares,
            )
            .ok_or(PoolError::OutOfRange)?;
            if out_a == 0 && out_b == 0 {
                return Err(PoolError::Nothing);
            }
            if out_a < *min_a || out_b < *min_b {
                return Err(PoolError::BelowMinimum);
            }
            for (asset, out) in [(pool.asset_a, out_a), (pool.asset_b, out_b)] {
                fits(stage.debit(address, &asset, out))?;
                fits(stage.credit(sender, &asset, out))?;
            }
            // The sender's half took the shares; here they cease to exist.
            pool.shares = pool
                .shares
                .checked_sub(*shares)
                .ok_or(PoolError::OutOfRange)?;
            stage.own.pools.insert(*address, pool);
            Ok(Outcome::LiquidityRemoved {
                amount_a: out_a,
                amount_b: out_b,
            })
        }
        Action::Swap {
            asset_in,
            amount_in,
            asset_out,
            min_out,
        } => {
            let address = pool_address(*asset_in, *asset_out);
            if stage.pool(&address).is_none_or(|pool| pool.shares == 0) {
                return Err(PoolError::NoPool);
            }
            let out = swap_output(
                stage.balance(&address, asset_in),
                stage.balance(&address, asset_out),
                *amount_in,
            )
            .ok_or(PoolError::OutOfRange)?;
            if out == 0 {
                return Err(PoolError::Nothing);
            }
            if out < *min_out {
                return Err(PoolError::BelowMinimum);
            }
            // The whole payment joins the reserves: the fee stays in the pool.
            fits(stage.credit(&address, asset_in, *amount_in))?;
            fits(stage.debit(&address, asset_out, out))?;
            fits(stage.credit(sender, asset_out, out))?;
            Ok(Outcome::Swapped { amount_out: out })
        }
        Action::Transfer { .. } | Action::IssueToken { .. } | Action::TransferToken { .. } => {
            Ok(Outcome::Applied)
        }
    }
}

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
            tokens: BTreeMap::new(),
            holdings: BTreeMap::new(),
            pools: BTreeMap::new(),
            last_outcomes: Vec::new(),
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

    /// How much of `asset` `holder` has.
    pub fn balance_of(&self, holder: &Address, asset: &Asset) -> Amount {
        match asset {
            Asset::Stein => self.accounts.get(holder).map_or(0, |a| a.balance),
            Asset::Token(token) => self.holdings.get(&(*holder, *token)).copied().unwrap_or(0),
        }
    }

    /// The tokens and pool shares `holder` has, by asset address.
    pub fn holdings_of(&self, holder: &Address) -> Vec<(Address, Amount)> {
        self.holdings
            .range((*holder, Address([0; 32]))..=(*holder, Address([0xff; 32])))
            .map(|((_, asset), amount)| (*asset, *amount))
            .collect()
    }

    /// A token, if `address` is one.
    pub fn token(&self, address: &Address) -> Option<&Token> {
        self.tokens.get(address)
    }

    /// Every token, in address order.
    pub fn tokens(&self) -> impl Iterator<Item = (&Address, &Token)> {
        self.tokens.iter()
    }

    /// A pool, if `address` is one.
    pub fn pool(&self, address: &Address) -> Option<&Pool> {
        self.pools.get(address)
    }

    /// Every pool, in address order.
    pub fn pools(&self) -> impl Iterator<Item = (&Address, &Pool)> {
        self.pools.iter()
    }

    /// What each transaction of the last block built or imported did, in
    /// block order. Derived by execution, so every node derives the same.
    pub fn last_outcomes(&self) -> &[Outcome] {
        &self.last_outcomes
    }

    /// The `limit` accounts with the largest balances, largest first. O(n):
    /// fine for a devnet; an indexer serves this at scale (SPEC §11.1).
    pub fn richest(&self, limit: usize) -> Vec<(Address, &Account)> {
        let mut all: Vec<(Address, &Account)> =
            self.accounts.iter().map(|(a, acc)| (*a, acc)).collect();
        all.sort_by(|x, y| y.1.balance.cmp(&x.1.balance).then(x.0.cmp(&y.0)));
        all.truncate(limit);
        all
    }

    /// Number of accounts that exist.
    pub fn account_count(&self) -> usize {
        self.accounts.len()
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

    /// Whether the sender's current balances cover `tx` on its own: what it
    /// sends, plus the fee. For mempool admission on the node that builds
    /// blocks, so accounts holding nothing cannot fill the pool for free. What
    /// the sender's other queued transactions spend is settled at execution.
    /// Cheap; run it before [`State::check_transaction`].
    pub fn check_funds(&self, tx: &Transaction) -> Result<(), TxError> {
        let sender = &tx.body.sender;
        let action = &tx.body.action;
        let stein = action
            .stein_debit()
            .checked_add(tx.body.fee)
            .ok_or(TxError::Overflow)?;
        let short = self.balance_of(sender, &Asset::Stein) < stein
            || action.debits().iter().any(|(asset, amount)| {
                *asset != Asset::Stein && self.balance_of(sender, asset) < *amount
            });
        if short {
            return Err(TxError::InsufficientBalance);
        }
        Ok(())
    }

    /// Executes `body` on top of `overlay` (falling back to committed state).
    /// Atomic: on error, `overlay` is untouched.
    ///
    /// Validity depends on the sender's state alone: its nonce, its key, and
    /// that it holds the fee and what it sends (SHARD-READY I-2). Everything
    /// done to another account is a credit, which cannot fail (I-1), or the
    /// pool's half of a pool operation, which answers with credits.
    fn apply(
        &self,
        overlay: &mut Overlay,
        body: &TxBody,
        attached_key: Option<&PublicKey>,
    ) -> Result<Outcome, TxError> {
        // Changes are staged and merged only on success. Sender, recipient
        // and fee pool may be the same account; staging through one view
        // keeps that correct.
        let mut stage = Stage {
            state: self,
            block: overlay,
            own: Overlay::default(),
        };

        let mut sender = stage.account(&body.sender);
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

        // The sender's half: the fee and everything sent leave the account.
        let sent = body.action.debits();
        let stein = body
            .action
            .stein_debit()
            .checked_add(body.fee)
            .ok_or(TxError::Overflow)?;
        sender.balance = sender
            .balance
            .checked_sub(stein)
            .ok_or(TxError::InsufficientBalance)?;
        sender.nonce = sender.nonce.checked_add(1).ok_or(TxError::Overflow)?;
        stage.own.accounts.insert(body.sender, sender);
        for (asset, amount) in &sent {
            if *asset != Asset::Stein {
                stage.debit(&body.sender, asset, *amount)?;
            }
        }

        // Credits come after the debits are staged, so a transfer to oneself
        // is just a fee payment.
        stage.credit(&stein_types::fee_pool(), &Asset::Stein, body.fee)?;
        let outcome = match &body.action {
            Action::Transfer { to, amount } => {
                stage.credit(to, &Asset::Stein, *amount)?;
                Outcome::Applied
            }
            Action::IssueToken {
                symbol,
                decimals,
                supply,
            } => {
                // The sender's nonce is spent by this transaction alone, so
                // the address is new.
                let token = Address::token(&body.sender, body.nonce);
                stage.own.tokens.insert(
                    token,
                    Token {
                        issuer: body.sender,
                        symbol: symbol.clone(),
                        decimals: *decimals,
                        supply: *supply,
                    },
                );
                stage.credit(&body.sender, &Asset::Token(token), *supply)?;
                Outcome::TokenIssued { token }
            }
            Action::TransferToken { token, to, amount } => {
                stage.credit(to, &Asset::Token(*token), *amount)?;
                Outcome::Applied
            }
            Action::AddLiquidity { .. } | Action::RemoveLiquidity { .. } | Action::Swap { .. } => {
                let before = stage.own.clone();
                match pool_step(&mut stage, body) {
                    Ok(outcome) => outcome,
                    Err(reason) => {
                        // The pool did nothing: what was sent comes back, as
                        // credits. The fee and the nonce stay spent.
                        stage.own = before;
                        for (asset, amount) in &sent {
                            stage.credit(&body.sender, asset, *amount)?;
                        }
                        Outcome::Refunded { reason }
                    }
                }
            }
        };
        let own = stage.own;
        overlay.extend(own);
        Ok(outcome)
    }

    /// At an epoch boundary, pays the fee pool out to the validators in
    /// proportion to voting power. The rounding remainder stays in the pool.
    /// Credits only: cannot fail, and every node computes the same split.
    fn pay_out_epoch(&self, overlay: &mut Overlay, height: Height) -> Result<(), TxError> {
        let epoch = self.params.epoch_blocks;
        if epoch == 0 || height.checked_rem(epoch) != Some(0) {
            return Ok(());
        }
        let pool_address = stein_types::fee_pool();
        let load = |overlay: &Overlay, a: &Address| {
            overlay
                .accounts
                .get(a)
                .or_else(|| self.accounts.get(a))
                .cloned()
                .unwrap_or_default()
        };
        let mut pool = load(overlay, &pool_address);
        let total_power: u64 = self
            .validators
            .iter()
            .map(|v| v.power)
            .fold(0, u64::saturating_add);
        if pool.balance == 0 || total_power == 0 {
            return Ok(());
        }
        let total = u128::from(total_power);
        let (Some(per_power), Some(remainder)) = (
            pool.balance.checked_div(total),
            pool.balance.checked_rem(total),
        ) else {
            return Ok(());
        };
        let mut paid: Amount = 0;
        for v in &self.validators {
            let power = u128::from(v.power);
            // (balance / total) * power + (balance % total) * power / total:
            // exact, and no intermediate exceeds the pool.
            let share = per_power
                .checked_mul(power)
                .and_then(|a| a.checked_add(remainder.checked_mul(power)?.checked_div(total)?))
                .ok_or(TxError::Overflow)?;
            let address = v.key.address();
            let mut account = load(overlay, &address);
            account.balance = account
                .balance
                .checked_add(share)
                .ok_or(TxError::Overflow)?;
            overlay.accounts.insert(address, account);
            paid = paid.checked_add(share).ok_or(TxError::Overflow)?;
        }
        // Re-read: a validator could in principle be the pool's own address.
        pool = load(overlay, &pool_address);
        pool.balance = pool.balance.checked_sub(paid).ok_or(TxError::Overflow)?;
        overlay.accounts.insert(pool_address, pool);
        Ok(())
    }

    /// Commits `overlay`, returning the previous values so the commit can be
    /// undone.
    fn commit(&mut self, overlay: Overlay) -> Undo {
        Undo {
            accounts: overlay
                .accounts
                .into_iter()
                .map(|(address, account)| (address, self.accounts.insert(address, account)))
                .collect(),
            holdings: overlay
                .holdings
                .into_iter()
                .map(|(key, amount)| {
                    let previous = if amount == 0 {
                        self.holdings.remove(&key)
                    } else {
                        self.holdings.insert(key, amount)
                    };
                    (key, previous)
                })
                .collect(),
            tokens: overlay
                .tokens
                .into_iter()
                .map(|(address, token)| (address, self.tokens.insert(address, token)))
                .collect(),
            pools: overlay
                .pools
                .into_iter()
                .map(|(address, pool)| (address, self.pools.insert(address, pool)))
                .collect(),
        }
    }

    fn undo(&mut self, undo: Undo) {
        let mut key_cache = self.key_cache.lock();
        for (address, _) in &undo.accounts {
            key_cache.remove(address);
        }
        drop(key_cache);
        restore(&mut self.accounts, undo.accounts);
        restore(&mut self.holdings, undo.holdings);
        restore(&mut self.tokens, undo.tokens);
        restore(&mut self.pools, undo.pools);
    }

    /// The state root. O(n) per block; the production design replaces it with
    /// an incrementally updated Jellyfish Merkle Tree (SPEC §5.4).
    ///
    /// While no token or pool exists it is the Merkle root over all accounts
    /// in address order, as it has been since genesis: a chain's history from
    /// before tokens replays to the same roots. From the first token on it
    /// commits to four trees: accounts, tokens, holdings and pools.
    fn compute_root(&self) -> Hash {
        let accounts: Vec<Hash> = self
            .accounts
            .par_iter()
            .map(|(address, account)| {
                hash(
                    ACCOUNT_LEAF,
                    &[&address.0, &borsh::to_vec(account).unwrap_or_default()],
                )
            })
            .collect();
        let accounts = merkle_root(&accounts);
        if self.tokens.is_empty() && self.holdings.is_empty() && self.pools.is_empty() {
            return accounts;
        }
        let tokens: Vec<Hash> = self
            .tokens
            .iter()
            .map(|(address, token)| {
                hash(
                    TOKEN_LEAF,
                    &[&address.0, &borsh::to_vec(token).unwrap_or_default()],
                )
            })
            .collect();
        let holdings: Vec<Hash> = self
            .holdings
            .par_iter()
            .map(|((holder, asset), amount)| {
                hash(HOLDING_LEAF, &[&holder.0, &asset.0, &amount.to_le_bytes()])
            })
            .collect();
        let pools: Vec<Hash> = self
            .pools
            .iter()
            .map(|(address, pool)| {
                hash(
                    POOL_LEAF,
                    &[&address.0, &borsh::to_vec(pool).unwrap_or_default()],
                )
            })
            .collect();
        hash(
            STATE_ROOT,
            &[
                &accounts.0,
                &merkle_root(&tokens).0,
                &merkle_root(&holdings).0,
                &merkle_root(&pools).0,
            ],
        )
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
        // Senders with a transaction that did not fit. Their later nonces
        // wait with it for the next block; they are not rejected.
        let mut deferred: HashSet<Address> = HashSet::new();
        for tx in candidates {
            if count >= self.params.max_block_txs {
                break;
            }
            if deferred.contains(&tx.body.sender) {
                continue;
            }
            let size = tx.encoded_len();
            if bytes.saturating_add(size) > self.params.max_block_bytes {
                deferred.insert(tx.body.sender);
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
        let mut overlay = Overlay::default();
        let mut bodies = Vec::new();
        let mut witnesses: Vec<Witness> = Vec::new();
        let mut outcomes = Vec::new();
        for ((tx, _), ok) in staged.into_iter().zip(valid) {
            if !ok {
                rejected.push((tx.id(), TxError::BadSignature));
                continue;
            }
            match self.apply(&mut overlay, &tx.body, tx.public_key()) {
                Ok(outcome) => {
                    bodies.push(tx.body);
                    witnesses.push(tx.witness);
                    outcomes.push(outcome);
                }
                Err(e) => rejected.push((tx.id(), e)),
            }
        }

        // Cannot fail with supply below 2^127; a failure here would be a bug,
        // so the block is built without the payout rather than not at all.
        let mut paid_out = overlay.clone();
        if self.pay_out_epoch(&mut paid_out, height).is_ok() {
            overlay = paid_out;
        }
        self.commit(overlay);
        let state_root = self.compute_root();
        let header = BlockHeader {
            version: BlockHeader::VERSION,
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
        self.last_outcomes = outcomes;
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
        if h.version != BlockHeader::VERSION {
            return Err(BlockError::UnsupportedVersion(h.version));
        }
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

        let mut overlay = Overlay::default();
        let mut outcomes = Vec::with_capacity(txs.len());
        for (i, tx) in txs.iter().enumerate() {
            let outcome = self
                .apply(&mut overlay, &tx.body, tx.public_key())
                .map_err(|e| invalid(i, e))?;
            outcomes.push(outcome);
        }
        // Exactly as in build_block: leader and followers must agree.
        let mut paid_out = overlay.clone();
        if self.pay_out_epoch(&mut paid_out, h.height).is_ok() {
            overlay = paid_out;
        }
        let undo = self.commit(overlay);
        let root = self.compute_root();
        if root != h.state_root {
            self.undo(undo);
            return Err(BlockError::StateRoot);
        }
        self.advance(h, root);
        self.last_outcomes = outcomes;
        Ok(())
    }
}

#[cfg(test)]
mod asset_tests;
#[cfg(test)]
mod tests;
