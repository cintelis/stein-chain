#![allow(clippy::arithmetic_side_effects)]

//! Property tests of the state machine (SPEC §9): random histories of
//! transfers, tokens, pools and swaps, valid and not, from three accounts.
//!
//! After every block, whatever the history:
//!
//! * nothing is created or lost: STEIN, every token and every pool's shares
//!   add up to their supply;
//! * a second node that verifies the block reaches the same state root;
//! * a block the proposer signed with a transaction slipped in, or with its
//!   witnesses withheld, is refused and leaves no trace;
//! * a node replaying the whole history from genesis ends in the same state.
//!
//! And no input, however extreme its amounts, makes the state machine panic.

use proptest::collection::vec;
use proptest::prelude::*;

use super::asset_tests::assert_conserved;
use super::tests::{CHAIN, key, net};
use super::*;
use stein_types::{Allocation, ONE_STEIN};

/// An amount, decided when the transaction is built: of what the sender
/// holds, or a fixed figure.
#[derive(Debug, Clone)]
enum Amt {
    /// Thousandths of the sender's balance; above 1000 is more than it has.
    PerMille(u128),
    Exactly(u128),
}

impl Amt {
    fn of(&self, balance: Amount) -> Amount {
        match self {
            // At least one unit of anything the sender holds at all.
            Self::PerMille(n) => (balance.saturating_mul(*n) / 1000).max(balance.min(1)),
            Self::Exactly(n) => *n,
        }
    }
}

fn amt() -> BoxedStrategy<Amt> {
    prop_oneof![
        30 => (1..=600u128).prop_map(Amt::PerMille),
        4 => (601..=1000u128).prop_map(Amt::PerMille),
        2 => (1001..=1200u128).prop_map(Amt::PerMille),
        4 => (1..5_000u128).prop_map(Amt::Exactly),
        1 => Just(Amt::Exactly(0)),
        1 => Just(Amt::Exactly(u128::MAX)),
        1 => Just(Amt::Exactly(stein_types::MAX_TOTAL_SUPPLY)),
        1 => any::<u128>().prop_map(Amt::Exactly),
    ]
    .boxed()
}

/// A lower bound the sender asks for: usually none, sometimes too much.
fn floor() -> BoxedStrategy<Amount> {
    prop_oneof![
        12 => Just(0),
        3 => 0..1_000_000u128,
        1 => any::<u128>(),
    ]
    .boxed()
}

/// An asset, decided against the state when the transaction is built.
#[derive(Debug, Clone)]
enum Pick {
    Stein,
    /// One of the tokens and pool shares the sender holds, by position;
    /// failing that, one that exists.
    Held(usize),
    /// One of the tokens and pools that exist, by position.
    Known(usize),
    /// An address nothing lives at.
    Nothing,
}

fn pick() -> BoxedStrategy<Pick> {
    prop_oneof![
        8 => Just(Pick::Stein),
        12 => (0..8usize).prop_map(Pick::Held),
        3 => (0..8usize).prop_map(Pick::Known),
        1 => Just(Pick::Nothing),
    ]
    .boxed()
}

#[derive(Debug, Clone)]
enum Op {
    Transfer {
        to: usize,
        amount: Amt,
    },
    Issue {
        supply: Amt,
        decimals: u8,
        good_symbol: bool,
    },
    SendToken {
        token: Pick,
        to: usize,
        amount: Amt,
    },
    Add {
        a: Pick,
        b: Pick,
        amount_a: Amt,
        amount_b: Amt,
        min_shares: Amount,
        /// Name the pair in canonical order, and as two different assets.
        well_formed: bool,
    },
    /// A swap through a pool that exists, one way or the other.
    Trade {
        pool: usize,
        forward: bool,
        amount: Amt,
        min_out: Amount,
    },
    Remove {
        pool: Pick,
        shares: Amt,
        min_a: Amount,
        min_b: Amount,
    },
    Swap {
        from: Pick,
        to: Pick,
        amount: Amt,
        min_out: Amount,
    },
}

/// Who a transfer goes to: an actor, a pool or token, or a stranger.
const RECIPIENTS: usize = 6;

fn op() -> BoxedStrategy<Op> {
    prop_oneof![
        3 => (0..RECIPIENTS, amt()).prop_map(|(to, amount)| Op::Transfer { to, amount }),
        2 => (amt(), 0..=19u8, prop::bool::weighted(0.95)).prop_map(
            |(supply, decimals, good_symbol)| Op::Issue { supply, decimals, good_symbol }
        ),
        2 => (pick(), 0..RECIPIENTS, amt())
            .prop_map(|(token, to, amount)| Op::SendToken { token, to, amount }),
        4 => (pick(), pick(), amt(), amt(), floor(), prop::bool::weighted(0.95)).prop_map(
            |(a, b, amount_a, amount_b, min_shares, well_formed)| Op::Add {
                a, b, amount_a, amount_b, min_shares, well_formed,
            }
        ),
        2 => (pick(), amt(), floor(), floor()).prop_map(|(pool, shares, min_a, min_b)| {
            Op::Remove { pool, shares, min_a, min_b }
        }),
        1 => (pick(), pick(), amt(), floor())
            .prop_map(|(from, to, amount, min_out)| Op::Swap { from, to, amount, min_out }),
        5 => (0..8usize, any::<bool>(), amt(), floor()).prop_map(
            |(pool, forward, amount, min_out)| Op::Trade { pool, forward, amount, min_out }
        ),
    ]
    .boxed()
}

/// One transaction to be: who sends it, what it does, and how it is wrong,
/// if it is.
#[derive(Debug, Clone)]
struct Step {
    who: usize,
    op: Op,
    /// Added to the right nonce; 0 almost always.
    nonce_skew: u64,
    fee: Amt,
    /// Attach the key when it should not be, or leave it off when it should.
    wrong_key_flag: bool,
    /// If `who` cannot afford this, let the next actor who can send it.
    /// Without it most random transactions are refused for lack of funds,
    /// and little of the state machine runs.
    by_whoever_can: bool,
}

fn step() -> BoxedStrategy<Step> {
    (
        0..ACTORS,
        op(),
        prop_oneof![39 => Just(0u64), 1 => 1..3u64],
        prop_oneof![
            48 => Just(Amt::Exactly(ONE_STEIN / 100)),
            1 => Just(Amt::Exactly(0)),
            1 => Just(Amt::PerMille(1100)),
        ],
        prop::bool::weighted(0.02),
        prop::bool::weighted(0.9),
    )
        .prop_map(
            |(who, op, nonce_skew, fee, wrong_key_flag, by_whoever_can)| Step {
                who,
                op,
                nonce_skew,
                fee,
                wrong_key_flag,
                by_whoever_can,
            },
        )
        .boxed()
}

const ACTORS: usize = 3;

struct World {
    validator: SecretKey,
    actors: Vec<SecretKey>,
    genesis: Genesis,
}

fn world() -> World {
    let n = net();
    let actors = vec![n.alice, n.bob, key(4)];
    let mut genesis = n.genesis;
    genesis.allocations = vec![
        Allocation {
            address: actors[0].address(),
            amount: 1_000 * ONE_STEIN,
        },
        Allocation {
            address: actors[1].address(),
            amount: 300 * ONE_STEIN,
        },
        // The third actor starts with nothing and must be paid before it
        // can act.
    ];
    World {
        validator: n.validator,
        actors,
        genesis,
    }
}

impl World {
    /// Tokens and pools that exist, in address order.
    fn known(state: &State) -> Vec<Address> {
        state
            .tokens
            .keys()
            .chain(state.pools.keys())
            .copied()
            .collect()
    }

    fn asset(state: &State, me: &Address, pick: &Pick) -> Asset {
        let known = Self::known(state);
        let held = state.holdings_of(me);
        match pick {
            Pick::Stein => Asset::Stein,
            Pick::Held(i) if !held.is_empty() => Asset::Token(held[i % held.len()].0),
            Pick::Held(i) | Pick::Known(i) if !known.is_empty() => {
                Asset::Token(known[i % known.len()])
            }
            _ => Asset::Token(Address([0xee; 32])),
        }
    }

    fn address(state: &State, me: &Address, pick: &Pick) -> Address {
        match Self::asset(state, me, pick) {
            Asset::Token(address) => address,
            Asset::Stein => Address([0xee; 32]),
        }
    }

    /// Two assets as a pair: different, unless there is only STEIN to be had.
    fn pair(state: &State, me: &Address, a: &Pick, b: &Pick) -> (Asset, Asset) {
        let (a, mut b) = (Self::asset(state, me, a), Self::asset(state, me, b));
        if a == b {
            b = match a {
                Asset::Token(_) => Asset::Stein,
                Asset::Stein => Self::asset(state, me, &Pick::Held(0)),
            };
        }
        (a, b)
    }

    fn recipient(&self, state: &State, to: usize) -> Address {
        let known = Self::known(state);
        match to {
            i if i < ACTORS => self.actors[i].address(),
            i if i == ACTORS && !known.is_empty() => known[0],
            i if i == ACTORS + 1 && !known.is_empty() => known[known.len() - 1],
            _ => Address([0xdd; 32]),
        }
    }

    fn action(&self, state: &State, me: &Address, op: &Op) -> Action {
        let held = |asset: &Asset| state.balance_of(me, asset);
        match op {
            Op::Transfer { to, amount } => Action::Transfer {
                to: self.recipient(state, *to),
                amount: amount.of(held(&Asset::Stein)),
            },
            Op::Issue {
                supply,
                decimals,
                good_symbol,
            } => Action::IssueToken {
                symbol: if *good_symbol { "TOK" } else { "tok" }.into(),
                decimals: *decimals,
                supply: supply.of(1_000_000_000),
            },
            Op::SendToken { token, to, amount } => {
                let token = Self::address(state, me, token);
                Action::TransferToken {
                    token,
                    to: self.recipient(state, *to),
                    amount: amount.of(held(&Asset::Token(token))),
                }
            }
            Op::Add {
                a,
                b,
                amount_a,
                amount_b,
                min_shares,
                well_formed,
            } => {
                let (mut a, mut b) = if *well_formed {
                    Self::pair(state, me, a, b)
                } else {
                    (Self::asset(state, me, a), Self::asset(state, me, b))
                };
                if (a > b) == *well_formed {
                    std::mem::swap(&mut a, &mut b);
                }
                Action::AddLiquidity {
                    asset_a: a,
                    amount_a: amount_a.of(held(&a)),
                    asset_b: b,
                    amount_b: amount_b.of(held(&b)),
                    min_shares: *min_shares,
                }
            }
            Op::Remove {
                pool,
                shares,
                min_a,
                min_b,
            } => {
                // Prefer a pool over a token, when there is one.
                let pool = match pool {
                    Pick::Known(i) if !state.pools.is_empty() => {
                        *state.pools.keys().nth(i % state.pools.len()).expect("pool")
                    }
                    other => Self::address(state, me, other),
                };
                Action::RemoveLiquidity {
                    pool,
                    shares: shares.of(held(&Asset::Token(pool))),
                    min_a: *min_a,
                    min_b: *min_b,
                }
            }
            Op::Swap {
                from,
                to,
                amount,
                min_out,
            } => {
                let asset_in = Self::asset(state, me, from);
                Action::Swap {
                    asset_in,
                    amount_in: amount.of(held(&asset_in)),
                    asset_out: Self::asset(state, me, to),
                    min_out: *min_out,
                }
            }
            Op::Trade {
                pool,
                forward,
                amount,
                min_out,
            } => {
                // Through a pool that exists; before there is one, between
                // STEIN and something the sender holds.
                let (a, b) = match state.pools.values().nth(pool % state.pools.len().max(1)) {
                    Some(pool) => (pool.asset_a, pool.asset_b),
                    None => Self::pair(state, me, &Pick::Stein, &Pick::Held(*pool)),
                };
                let (asset_in, asset_out) = if *forward { (a, b) } else { (b, a) };
                Action::Swap {
                    asset_in,
                    amount_in: amount.of(held(&asset_in)),
                    asset_out,
                    min_out: *min_out,
                }
            }
        }
    }

    /// Whether `who` holds what `step` spends, fee included.
    fn can_afford(&self, state: &State, who: usize, step: &Step) -> bool {
        let me = self.actors[who].address();
        let stein = state.balance_of(&me, &Asset::Stein);
        let action = self.action(state, &me, &step.op);
        let covered = action
            .debits()
            .iter()
            .all(|(asset, amount)| *amount > 0 && *amount <= state.balance_of(&me, asset));
        covered
            && step
                .fee
                .of(stein)
                .checked_add(action.stein_debit())
                .is_some_and(|total| total <= stein)
    }

    /// Signs the block's transactions against `state` as it stands, counting
    /// on each sender's earlier transactions in the block being included.
    fn transactions(&self, state: &State, steps: &[Step]) -> Vec<Transaction> {
        let mut queued = [0u64; ACTORS];
        steps
            .iter()
            .map(|step| {
                let sender = (0..ACTORS)
                    .map(|k| (step.who + k) % ACTORS)
                    .find(|who| step.by_whoever_can && self.can_afford(state, *who, step))
                    .unwrap_or(step.who);
                let who = &self.actors[sender];
                let me = who.address();
                let account = state.account(&me);
                let nonce = account.map_or(0, |a| a.nonce) + queued[sender] + step.nonce_skew;
                let registered = account.is_some_and(|a| a.public_key.is_some());
                let register = (!registered && queued[sender] == 0) != step.wrong_key_flag;
                queued[sender] += 1;
                let body = TxBody {
                    version: TxBody::VERSION,
                    chain_id: CHAIN,
                    sender: me,
                    nonce,
                    fee: step.fee.of(state.balance_of(&me, &Asset::Stein)),
                    expires_at: 100,
                    action: self.action(state, &me, &step.op),
                };
                Transaction::sign(body, who, register).expect("sign")
            })
            .collect()
    }

    /// `block` with `extra` appended and the roots that go with it, signed
    /// by the validator: what a proposer who slips a transaction in would
    /// send. The state root is the honest block's.
    fn with_extra(&self, block: &Block, extra: Transaction) -> Block {
        let mut bodies = block.bodies.clone();
        let mut witnesses = block.witnesses.clone().expect("witnesses");
        bodies.push(extra.body);
        witnesses.push(extra.witness);
        self.signed(
            BlockHeader {
                tx_count: bodies.len() as u32,
                tx_root: Block::tx_root(&bodies),
                witness_root: Block::witness_root(&witnesses),
                ..block.header.clone()
            },
            bodies,
            Some(witnesses),
        )
    }

    fn signed(
        &self,
        header: BlockHeader,
        bodies: Vec<TxBody>,
        witnesses: Option<Vec<Witness>>,
    ) -> Block {
        let proposer_signature = self
            .validator
            .sign(Domain::Proposal, &header.encode())
            .expect("sign");
        Block {
            header,
            proposer_signature,
            bodies,
            witnesses,
        }
    }
}

/// Everything about a node's state that a refused block must leave alone.
fn fingerprint(state: &State) -> (Hash, Hash, Height, Hash) {
    (
        state.compute_root(),
        state.state_root(),
        state.height(),
        state.head(),
    )
}

/// A block that must be refused, and must leave `state` as it was.
fn assert_refused(state: &mut State, block: &Block, what: &str) -> Result<(), TestCaseError> {
    let before = fingerprint(state);
    prop_assert!(state.import_block(block).is_err(), "accepted {}", what);
    prop_assert_eq!(fingerprint(state), before, "{} left a trace", what);
    Ok(())
}

proptest! {
    // Each case signs and verifies a few dozen transactions on three nodes.
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|n| n.parse().ok()).unwrap_or(48),
        ..ProptestConfig::default()
    })]

    #[test]
    fn any_history_conserves_everything_and_every_node_agrees(
        history in vec(vec(step(), 1..5), 1..16),
    ) {
        let w = world();
        let mut leader = State::from_genesis(&w.genesis).expect("genesis");
        let mut follower = State::from_genesis(&w.genesis).expect("genesis");
        let mut blocks = Vec::new();

        for (round, steps) in history.iter().enumerate() {
            let txs = w.transactions(&leader, steps);
            let nonces_before: Vec<u64> = w
                .actors
                .iter()
                .map(|k| leader.account(&k.address()).map_or(0, |a| a.nonce))
                .collect();
            let timestamp = 2_000 + leader.height() * 1_000;
            let (block, rejected) = leader
                .build_block(txs.clone(), &w.validator, timestamp)
                .expect("build");

            // Every transaction is in the block or was refused, never both.
            prop_assert_eq!(block.bodies.len() + rejected.len(), txs.len(), "round {}", round);
            assert_conserved(&leader, &w.genesis);
            // Each included transaction used exactly one nonce of its sender.
            for (i, key) in w.actors.iter().enumerate() {
                let included = block.bodies.iter().filter(|b| b.sender == key.address()).count();
                let nonce = leader.account(&key.address()).map_or(0, |a| a.nonce);
                prop_assert_eq!(nonce, nonces_before[i] + included as u64, "round {}", round);
            }

            // What a dishonest proposer could sign instead is refused, and
            // leaves the follower as it was.
            assert_refused(
                &mut follower,
                &w.signed(block.header.clone(), block.bodies.clone(), None),
                "a block without witnesses",
            )?;
            for tx in txs.iter().filter(|tx| rejected.iter().any(|(id, _)| *id == tx.id())) {
                assert_refused(
                    &mut follower,
                    &w.with_extra(&block, tx.clone()),
                    "a block with a refused transaction slipped in",
                )?;
            }
            let mut wrong_root = block.header.clone();
            wrong_root.state_root = Hash([0x5a; 32]);
            assert_refused(
                &mut follower,
                &w.signed(wrong_root, block.bodies.clone(), block.witnesses.clone()),
                "a block with another state root",
            )?;

            follower.import_block(&block).expect("the honest block");
            prop_assert_eq!(follower.state_root(), leader.state_root(), "round {}", round);
            prop_assert_eq!(follower.compute_root(), leader.compute_root(), "round {}", round);
            assert_conserved(&follower, &w.genesis);

            // A valid transaction on top of the block, under the block's own
            // state root: applied, found out by the root, and undone.
            let richest = (0..ACTORS)
                .max_by_key(|i| leader.balance_of(&w.actors[*i].address(), &Asset::Stein))
                .expect("an actor");
            let extra = w.transactions(&leader, &[Step {
                who: richest,
                op: Op::Transfer { to: (richest + 1) % ACTORS, amount: Amt::Exactly(1) },
                nonce_skew: 0,
                fee: Amt::Exactly(ONE_STEIN / 100),
                wrong_key_flag: false,
                by_whoever_can: false,
            }]);
            let next = w.signed(
                BlockHeader {
                    height: block.header.height + 1,
                    parent: block.hash(),
                    timestamp_ms: block.header.timestamp_ms + 1_000,
                    tx_count: 1,
                    tx_root: Block::tx_root(&[extra[0].body.clone()]),
                    witness_root: Block::witness_root(&[extra[0].witness.clone()]),
                    ..block.header.clone()
                },
                vec![extra[0].body.clone()],
                Some(vec![extra[0].witness.clone()]),
            );
            assert_refused(&mut follower, &next, "a block whose state root is the last block's")?;

            blocks.push(block);
        }

        // A node that starts later, from the log alone, ends in the same state.
        let mut late = State::from_genesis(&w.genesis).expect("genesis");
        for block in &blocks {
            late.import_block(block).expect("replay");
        }
        prop_assert_eq!(fingerprint(&late), fingerprint(&leader));
    }
}
