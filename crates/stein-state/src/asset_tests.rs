#![allow(clippy::arithmetic_side_effects)]

//! Tokens and liquidity pools (SPEC §4.6, §4.7).

use super::tests::{CHAIN, Net, net, produce, transfer};
use super::*;
use stein_types::ONE_STEIN;

const FEE: Amount = ONE_STEIN / 100;
const STEIN: Asset = Asset::Stein;

fn act(from: &SecretKey, action: Action, nonce: u64, register: bool) -> Transaction {
    let body = TxBody {
        version: TxBody::VERSION,
        chain_id: CHAIN,
        sender: from.address(),
        nonce,
        fee: FEE,
        expires_at: 100,
        action,
    };
    Transaction::sign(body, from, register).expect("sign")
}

fn issue(symbol: &str, supply: Amount) -> Action {
    Action::IssueToken {
        symbol: symbol.into(),
        decimals: 0,
        supply,
    }
}

fn add(asset_a: Asset, amount_a: Amount, asset_b: Asset, amount_b: Amount) -> Action {
    Action::AddLiquidity {
        asset_a,
        amount_a,
        asset_b,
        amount_b,
        min_shares: 0,
    }
}

fn swap(asset_in: Asset, amount_in: Amount, asset_out: Asset, min_out: Amount) -> Action {
    Action::Swap {
        asset_in,
        amount_in,
        asset_out,
        min_out,
    }
}

/// Every unit of everything is somewhere, and nothing is held in zero amounts.
fn assert_conserved(state: &State, genesis: &Genesis) {
    let stein: Amount = state.accounts.values().map(|a| a.balance).sum();
    assert_eq!(Some(stein), genesis.total_supply(), "STEIN supply");
    let held = |asset: &Address| -> Amount {
        state
            .holdings
            .iter()
            .filter(|((_, a), _)| a == asset)
            .map(|(_, amount)| *amount)
            .sum()
    };
    for (address, token) in &state.tokens {
        assert_eq!(held(address), token.supply, "supply of {}", token.symbol);
    }
    for (address, pool) in &state.pools {
        assert_eq!(held(address), pool.shares, "shares of pool {address}");
    }
    assert!(state.holdings.values().all(|amount| *amount > 0));
}

/// Alice has issued 1,000,000 GOLD and funded a STEIN/GOLD pool with
/// 100 STEIN : 400,000 GOLD; Bob holds 50 STEIN.
struct Market {
    n: Net,
    state: State,
    gold: Address,
    pool: Address,
    /// The blocks so far, for a second validator to verify.
    blocks: Vec<Block>,
}

impl Market {
    /// Builds a block on the market's state and keeps it.
    fn produce(&mut self, txs: Vec<Transaction>) -> (Block, Vec<(Hash, TxError)>) {
        let (block, rejected) = produce(&mut self.state, &self.n, txs);
        self.blocks.push(block.clone());
        (block, rejected)
    }

    /// Another validator that has verified and applied every block so far.
    fn follower(&self) -> State {
        let mut state = State::from_genesis(&self.n.genesis).expect("genesis");
        for block in &self.blocks {
            state.import_block(block).expect("import");
        }
        state
    }
}

const POOL_STEIN: Amount = 100 * ONE_STEIN;
const POOL_GOLD: Amount = 400_000;
/// ⌊√(10^11 × 400,000)⌋
const FIRST_SHARES: Amount = 200_000_000;

fn market() -> Market {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let gold = Address::token(&n.alice.address(), 0);
    let txs = vec![
        act(&n.alice, issue("GOLD", 1_000_000), 0, true),
        act(
            &n.alice,
            add(STEIN, POOL_STEIN, Asset::Token(gold), POOL_GOLD),
            1,
            false,
        ),
        transfer(&n.alice, n.bob.address(), 50 * ONE_STEIN, 2, false),
    ];
    let (block, rejected) = produce(&mut state, &n, txs);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(block.bodies.len(), 3);
    let pool = pool_address(STEIN, Asset::Token(gold));
    Market {
        n,
        state,
        gold,
        pool,
        blocks: vec![block],
    }
}

#[test]
fn issuing_a_token_credits_its_whole_supply_to_the_issuer() {
    let n = net();
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let plain_root = leader.state_root();

    let tx = act(
        &n.alice,
        Action::IssueToken {
            symbol: "GOLD".into(),
            decimals: 6,
            supply: 5_000,
        },
        0,
        true,
    );
    let (block, rejected) = produce(&mut leader, &n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");

    let alice = n.alice.address();
    let gold = Address::token(&alice, 0);
    assert_eq!(
        leader.last_outcomes(),
        [Outcome::TokenIssued { token: gold }]
    );
    assert_eq!(
        leader.token(&gold),
        Some(&Token {
            issuer: alice,
            symbol: "GOLD".into(),
            decimals: 6,
            supply: 5_000,
        })
    );
    assert_eq!(leader.balance_of(&alice, &Asset::Token(gold)), 5_000);
    assert_eq!(leader.holdings_of(&alice), vec![(gold, 5_000)]);
    // It cost the fee and nothing else.
    assert_eq!(leader.balance_of(&alice, &STEIN), 1_000 * ONE_STEIN - FEE);
    assert_ne!(leader.state_root(), plain_root);
    assert_conserved(&leader, &n.genesis);

    // Every validator derives the same token, balance and root.
    follower.import_block(&block).expect("import");
    assert_eq!(follower.state_root(), leader.state_root());
    assert_eq!(follower.last_outcomes(), leader.last_outcomes());
    assert_eq!(follower.token(&gold), leader.token(&gold));

    // The same issuer's next token is another token.
    let (_, rejected) = produce(
        &mut leader,
        &n,
        vec![act(&n.alice, issue("GOLD", 7), 1, false)],
    );
    assert!(rejected.is_empty(), "{rejected:?}");
    let second = Address::token(&alice, 1);
    assert_ne!(second, gold);
    assert_eq!(leader.balance_of(&alice, &Asset::Token(second)), 7);
    assert_eq!(leader.tokens().count(), 2);
}

#[test]
fn the_state_root_commits_to_tokens_and_holdings() {
    // Same accounts, different token state: different roots.
    let n = net();
    let build = |action: Action| {
        let mut state = State::from_genesis(&n.genesis).expect("genesis");
        produce(&mut state, &n, vec![act(&n.alice, action, 0, true)]);
        state
    };
    let a = build(issue("GOLD", 5_000));
    let b = build(issue("GOLD", 5_001));
    let c = build(issue("LEAD", 5_000));
    assert_eq!(a.accounts, b.accounts);
    assert_ne!(a.state_root(), b.state_root());
    assert_ne!(a.state_root(), c.state_root());

    // And a holding moving between accounts changes it.
    let mut moved = build(issue("GOLD", 5_000));
    let gold = Address::token(&n.alice.address(), 0);
    let before = moved.state_root();
    let accounts = moved.accounts.clone();
    moved.holdings.insert((n.bob.address(), gold), 5_000);
    moved.holdings.remove(&(n.alice.address(), gold));
    assert_eq!(moved.accounts, accounts);
    assert_ne!(moved.compute_root(), before);
}

#[test]
fn tokens_move_between_accounts_and_cannot_be_overspent() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let (alice, bob) = (n.alice.address(), n.bob.address());
    let gold = Address::token(&alice, 0);
    let send = |amount: Amount, nonce: u64| {
        act(
            &n.alice,
            Action::TransferToken {
                token: gold,
                to: bob,
                amount,
            },
            nonce,
            false,
        )
    };
    let (_, rejected) = produce(
        &mut state,
        &n,
        vec![
            act(&n.alice, issue("GOLD", 5_000), 0, true),
            send(1_200, 1),
            // More than is left: left out of the block, like an overdraft.
            send(3_801, 2),
        ],
    );
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].1, TxError::InsufficientBalance);

    assert_eq!(state.balance_of(&alice, &Asset::Token(gold)), 3_800);
    assert_eq!(state.balance_of(&bob, &Asset::Token(gold)), 1_200);
    // Bob holds tokens without ever having held STEIN.
    assert!(state.account(&bob).is_none());
    assert_eq!(state.account(&alice).expect("alice").nonce, 2);

    // Admission refuses what the sender cannot cover, in any asset.
    assert_eq!(
        state.check_funds(&send(3_801, 2)),
        Err(TxError::InsufficientBalance)
    );
    assert_eq!(state.check_funds(&send(3_800, 2)), Ok(()));
    let unknown = Action::TransferToken {
        token: Address([9; 32]),
        to: bob,
        amount: 1,
    };
    assert_eq!(
        state.check_funds(&act(&n.alice, unknown, 2, false)),
        Err(TxError::InsufficientBalance)
    );

    // Sending everything leaves no empty holding behind.
    let (_, rejected) = produce(&mut state, &n, vec![send(3_800, 2)]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert!(state.holdings_of(&alice).is_empty());
    assert_eq!(state.balance_of(&bob, &Asset::Token(gold)), 5_000);
    assert_conserved(&state, &n.genesis);
}

#[test]
fn malformed_asset_actions_are_refused_before_anything_else() {
    let n = net();
    let state = State::from_genesis(&n.genesis).expect("genesis");
    let token = |b: u8| Asset::Token(Address([b; 32]));
    let cases: Vec<(Action, TxError)> = vec![
        (issue("gold", 5), TxError::BadSymbol),
        (issue("STEIN", 5), TxError::BadSymbol),
        (issue("GOLD", 0), TxError::ZeroAmount),
        (
            issue("GOLD", stein_types::MAX_TOTAL_SUPPLY + 1),
            TxError::SupplyTooLarge,
        ),
        (
            Action::IssueToken {
                symbol: "GOLD".into(),
                decimals: 19,
                supply: 5,
            },
            TxError::BadDecimals,
        ),
        (
            Action::TransferToken {
                token: Address([1; 32]),
                to: n.bob.address(),
                amount: 0,
            },
            TxError::ZeroAmount,
        ),
        (add(token(2), 5, token(1), 5), TxError::AssetOrder),
        (add(token(1), 5, STEIN, 5), TxError::AssetOrder),
        (add(STEIN, 5, STEIN, 5), TxError::AssetOrder),
        (add(STEIN, 0, token(1), 5), TxError::ZeroAmount),
        (add(STEIN, 5, token(1), 0), TxError::ZeroAmount),
        (swap(token(1), 5, token(1), 0), TxError::AssetOrder),
        (swap(STEIN, 0, token(1), 0), TxError::ZeroAmount),
        (
            Action::RemoveLiquidity {
                pool: Address([1; 32]),
                shares: 0,
                min_a: 0,
                min_b: 0,
            },
            TxError::ZeroAmount,
        ),
    ];
    for (action, error) in cases {
        let tx = act(&n.alice, action.clone(), 0, true);
        assert_eq!(state.check_transaction(&tx, None), Err(error), "{action:?}");
    }
    // The largest supply that keeps credits from ever overflowing is fine.
    let most = act(
        &n.alice,
        issue("GOLD", stein_types::MAX_TOTAL_SUPPLY),
        0,
        true,
    );
    assert_eq!(state.check_transaction(&most, None), Ok(()));
}

#[test]
fn the_first_deposit_creates_the_pool_and_locks_the_minimum() {
    let m = market();
    let alice = m.n.alice.address();
    let share = Asset::Token(m.pool);

    assert_eq!(
        m.state.pool(&m.pool),
        Some(&Pool {
            asset_a: STEIN,
            asset_b: Asset::Token(m.gold),
            shares: FIRST_SHARES,
        })
    );
    // The reserves are the pool's own balances.
    assert_eq!(m.state.balance_of(&m.pool, &STEIN), POOL_STEIN);
    assert_eq!(
        m.state.balance_of(&m.pool, &Asset::Token(m.gold)),
        POOL_GOLD
    );
    // The pool keeps the locked shares; no key controls its address.
    assert_eq!(m.state.balance_of(&m.pool, &share), MIN_LIQUIDITY);
    assert_eq!(
        m.state.balance_of(&alice, &share),
        FIRST_SHARES - MIN_LIQUIDITY
    );
    assert!(m.state.account(&m.pool).expect("pool").public_key.is_none());
    assert_eq!(
        m.state.last_outcomes()[1],
        Outcome::LiquidityAdded {
            pool: m.pool,
            shares: FIRST_SHARES - MIN_LIQUIDITY,
            used_a: POOL_STEIN,
            used_b: POOL_GOLD,
        }
    );
    assert_eq!(
        m.state.balance_of(&alice, &STEIN),
        1_000 * ONE_STEIN - POOL_STEIN - 50 * ONE_STEIN - 3 * FEE
    );
    assert_conserved(&m.state, &m.n.genesis);
}

#[test]
fn a_first_deposit_too_small_to_leave_shares_is_returned() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let gold = Asset::Token(Address::token(&n.alice.address(), 0));
    // √(1,000 × 1,000) = 1,000 shares: all of them would be locked.
    let (block, rejected) = produce(
        &mut state,
        &n,
        vec![
            act(&n.alice, issue("GOLD", 1_000_000), 0, true),
            act(&n.alice, add(STEIN, 1_000, gold, 1_000), 1, false),
        ],
    );
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(block.bodies.len(), 2);
    assert_eq!(
        state.last_outcomes()[1],
        Outcome::Refunded {
            reason: PoolError::TooLittleLiquidity
        }
    );
    assert_eq!(state.pools().count(), 0);
    assert_eq!(state.balance_of(&n.alice.address(), &gold), 1_000_000);
    assert_eq!(
        state.balance_of(&n.alice.address(), &STEIN),
        1_000 * ONE_STEIN - 2 * FEE
    );
    assert_conserved(&state, &n.genesis);
}

#[test]
fn a_swap_trades_at_the_pool_price_and_the_fee_stays_in_the_pool() {
    let mut m = market();
    let bob = m.n.bob.address();
    let gold = Asset::Token(m.gold);
    let paid = 10 * ONE_STEIN;
    // 0.3% fee: 9.97 STEIN trade against 100 STEIN : 400,000 GOLD.
    let bought = 36_264;
    assert_eq!(swap_output(POOL_STEIN, POOL_GOLD, paid), Some(bought));

    let tx = act(&m.n.bob, swap(STEIN, paid, gold, bought), 0, true);
    let (_, rejected) = m.produce(vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        m.state.last_outcomes(),
        [Outcome::Swapped { amount_out: bought }]
    );
    assert_eq!(m.state.balance_of(&bob, &gold), bought);
    assert_eq!(
        m.state.balance_of(&bob, &STEIN),
        50 * ONE_STEIN - paid - FEE
    );
    // All of what was paid is in the reserves, fee included.
    assert_eq!(m.state.balance_of(&m.pool, &STEIN), POOL_STEIN + paid);
    assert_eq!(m.state.balance_of(&m.pool, &gold), POOL_GOLD - bought);
    assert!((POOL_STEIN + paid) * (POOL_GOLD - bought) > POOL_STEIN * POOL_GOLD);
    // Shares are untouched: the fee makes each one worth more.
    assert_eq!(m.state.pool(&m.pool).expect("pool").shares, FIRST_SHARES);
    assert_conserved(&m.state, &m.n.genesis);

    // Another validator, verifying both blocks, agrees.
    assert_eq!(m.follower().state_root(), m.state.state_root());

    // And back the other way: selling the GOLD returns less STEIN than paid.
    let tx = act(&m.n.bob, swap(gold, bought, STEIN, 0), 1, false);
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    let Outcome::Swapped { amount_out } = m.state.last_outcomes()[0] else {
        panic!("not swapped: {:?}", m.state.last_outcomes());
    };
    assert!(amount_out < paid, "a round trip pays the fee twice");
    assert_eq!(m.state.balance_of(&bob, &gold), 0);
    assert_conserved(&m.state, &m.n.genesis);
}

#[test]
fn a_swap_the_pool_cannot_fill_is_still_valid_and_returns_what_was_paid() {
    let mut m = market();
    let bob = m.n.bob.address();
    let gold = Asset::Token(m.gold);
    let silver = Asset::Token(Address([7; 32]));
    let pool = m.pool;
    let reserves = |state: &State| {
        (
            state.balance_of(&pool, &STEIN),
            state.balance_of(&pool, &gold),
        )
    };
    let before = reserves(&m.state);

    let txs = vec![
        // Asks for one more than the pool pays.
        act(&m.n.bob, swap(STEIN, 10 * ONE_STEIN, gold, 36_265), 0, true),
        // A pair with no pool.
        act(&m.n.bob, swap(STEIN, ONE_STEIN, silver, 0), 1, false),
        // Too little to buy anything.
        act(&m.n.bob, swap(STEIN, 1, gold, 0), 2, false),
    ];
    let (block, rejected) = m.produce(txs);
    // What the pool does is not a condition of validity (SHARD-READY I-2):
    // all three are in the block, and all three paid their fee.
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(block.bodies.len(), 3);
    assert_eq!(
        m.state.last_outcomes(),
        [
            Outcome::Refunded {
                reason: PoolError::BelowMinimum
            },
            Outcome::Refunded {
                reason: PoolError::NoPool
            },
            Outcome::Refunded {
                reason: PoolError::Nothing
            },
        ]
    );
    assert_eq!(m.state.balance_of(&bob, &STEIN), 50 * ONE_STEIN - 3 * FEE);
    assert_eq!(m.state.balance_of(&bob, &gold), 0);
    assert_eq!(m.state.account(&bob).expect("bob").nonce, 3);
    assert_eq!(reserves(&m.state), before);
    assert_eq!(m.state.pools().count(), 1);
    assert_conserved(&m.state, &m.n.genesis);

    let follower = m.follower();
    assert_eq!(follower.state_root(), m.state.state_root());
    assert_eq!(follower.last_outcomes(), m.state.last_outcomes());
}

#[test]
fn a_swap_needs_the_sender_to_hold_what_it_pays() {
    let mut m = market();
    let gold = Asset::Token(m.gold);
    // Bob has 50 STEIN and no GOLD. These fail on Bob's own state, so they
    // are invalid, not refunded: Bob pays nothing and his nonce stands.
    for action in [
        swap(gold, 5, STEIN, 0),
        // 50 STEIN and the fee is more than 50 STEIN.
        swap(STEIN, 50 * ONE_STEIN, gold, 0),
    ] {
        let tx = act(&m.n.bob, action, 0, true);
        assert_eq!(m.state.check_funds(&tx), Err(TxError::InsufficientBalance));
        let (block, rejected) = produce(&mut m.state, &m.n, vec![tx]);
        assert_eq!(block.bodies.len(), 0);
        assert_eq!(rejected[0].1, TxError::InsufficientBalance);
    }
    assert_eq!(
        m.state.balance_of(&m.n.bob.address(), &STEIN),
        50 * ONE_STEIN
    );
    assert!(
        m.state
            .account(&m.n.bob.address())
            .expect("bob")
            .public_key
            .is_none()
    );
}

#[test]
fn later_deposits_take_the_pool_ratio_and_return_the_rest() {
    let mut m = market();
    let alice = m.n.alice.address();
    let gold = Asset::Token(m.gold);
    let share = Asset::Token(m.pool);
    let (stein_before, gold_before) = (
        m.state.balance_of(&alice, &STEIN),
        m.state.balance_of(&alice, &gold),
    );

    // 10 STEIN : 100,000 GOLD offered at a ratio of 1 : 4,000 GOLD per STEIN.
    // STEIN is the scarce side: 10% more shares, 40,000 GOLD taken.
    let tx = act(
        &m.n.alice,
        add(STEIN, 10 * ONE_STEIN, gold, 100_000),
        3,
        false,
    );
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        m.state.last_outcomes(),
        [Outcome::LiquidityAdded {
            pool: m.pool,
            shares: FIRST_SHARES / 10,
            used_a: 10 * ONE_STEIN,
            used_b: 40_000,
        }]
    );
    assert_eq!(
        m.state.balance_of(&alice, &STEIN),
        stein_before - 10 * ONE_STEIN - FEE
    );
    assert_eq!(m.state.balance_of(&alice, &gold), gold_before - 40_000);
    assert_eq!(
        m.state.balance_of(&alice, &share),
        FIRST_SHARES - MIN_LIQUIDITY + FIRST_SHARES / 10
    );
    assert_eq!(
        m.state.pool(&m.pool).expect("pool").shares,
        FIRST_SHARES + FIRST_SHARES / 10
    );
    assert_conserved(&m.state, &m.n.genesis);

    // A deposit that would mint fewer shares than asked for is returned whole.
    let tx = act(
        &m.n.alice,
        Action::AddLiquidity {
            asset_a: STEIN,
            amount_a: ONE_STEIN,
            asset_b: gold,
            amount_b: 4_000,
            min_shares: FIRST_SHARES,
        },
        4,
        false,
    );
    let shares_before = m.state.balance_of(&alice, &share);
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        m.state.last_outcomes(),
        [Outcome::Refunded {
            reason: PoolError::BelowMinimum
        }]
    );
    assert_eq!(m.state.balance_of(&alice, &share), shares_before);
    assert_eq!(m.state.balance_of(&alice, &gold), gold_before - 40_000);
    assert_conserved(&m.state, &m.n.genesis);
}

#[test]
fn shares_redeem_for_their_part_of_both_reserves() {
    let mut m = market();
    let alice = m.n.alice.address();
    let gold = Asset::Token(m.gold);
    let share = Asset::Token(m.pool);
    let remove = |shares: Amount, min_a: Amount, nonce: u64| {
        act(
            &m.n.alice,
            Action::RemoveLiquidity {
                pool: m.pool,
                shares,
                min_a,
                min_b: 0,
            },
            nonce,
            false,
        )
    };
    let (stein_before, gold_before) = (
        m.state.balance_of(&alice, &STEIN),
        m.state.balance_of(&alice, &gold),
    );

    // A quarter of all shares: a quarter of each reserve.
    let quarter = FIRST_SHARES / 4;
    let tx = remove(quarter, 0, 3);
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        m.state.last_outcomes(),
        [Outcome::LiquidityRemoved {
            amount_a: POOL_STEIN / 4,
            amount_b: POOL_GOLD / 4,
        }]
    );
    assert_eq!(
        m.state.balance_of(&alice, &STEIN),
        stein_before + POOL_STEIN / 4 - FEE
    );
    assert_eq!(
        m.state.balance_of(&alice, &gold),
        gold_before + POOL_GOLD / 4
    );
    assert_eq!(
        m.state.pool(&m.pool).expect("pool").shares,
        FIRST_SHARES - quarter
    );
    assert_conserved(&m.state, &m.n.genesis);

    // Asking for more STEIN than the shares are worth: the shares come back.
    let held = m.state.balance_of(&alice, &share);
    let tx = remove(quarter, POOL_STEIN, 4);
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        m.state.last_outcomes(),
        [Outcome::Refunded {
            reason: PoolError::BelowMinimum
        }]
    );
    assert_eq!(m.state.balance_of(&alice, &share), held);

    // Redeeming every share Alice has still leaves the locked minimum, so
    // the pool is never emptied and keeps a price.
    let tx = remove(held, 0, 5);
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(m.state.balance_of(&alice, &share), 0);
    assert_eq!(m.state.pool(&m.pool).expect("pool").shares, MIN_LIQUIDITY);
    assert!(m.state.balance_of(&m.pool, &STEIN) > 0);
    assert!(m.state.balance_of(&m.pool, &gold) > 0);
    assert_conserved(&m.state, &m.n.genesis);

    // More shares than held is the sender's own shortfall: invalid.
    let (block, rejected) = produce(&mut m.state, &m.n, vec![remove(1, 0, 6)]);
    assert_eq!(block.bodies.len(), 0);
    assert_eq!(rejected[0].1, TxError::InsufficientBalance);
}

#[test]
fn shares_are_held_and_sent_like_a_token() {
    let mut m = market();
    let (alice, bob) = (m.n.alice.address(), m.n.bob.address());
    let share = Asset::Token(m.pool);
    let given = 50_000_000;
    let txs = vec![
        act(
            &m.n.alice,
            Action::TransferToken {
                token: m.pool,
                to: bob,
                amount: given,
            },
            3,
            false,
        ),
        // Bob redeems what he was given.
        act(
            &m.n.bob,
            Action::RemoveLiquidity {
                pool: m.pool,
                shares: given,
                min_a: 0,
                min_b: 0,
            },
            0,
            true,
        ),
    ];
    let (_, rejected) = produce(&mut m.state, &m.n, txs);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        m.state.balance_of(&alice, &share),
        FIRST_SHARES - MIN_LIQUIDITY - given
    );
    assert_eq!(m.state.balance_of(&bob, &share), 0);
    assert_eq!(
        m.state.balance_of(&bob, &Asset::Token(m.gold)),
        POOL_GOLD / 4
    );
    assert_eq!(
        m.state.balance_of(&bob, &STEIN),
        50 * ONE_STEIN + POOL_STEIN / 4 - FEE
    );
    assert_conserved(&m.state, &m.n.genesis);

    // Redeeming a token that is not a pool's shares gets the token back.
    let tx = act(
        &m.n.bob,
        Action::RemoveLiquidity {
            pool: m.gold,
            shares: 10,
            min_a: 0,
            min_b: 0,
        },
        1,
        false,
    );
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        m.state.last_outcomes(),
        [Outcome::Refunded {
            reason: PoolError::NoPool
        }]
    );
    assert_eq!(
        m.state.balance_of(&bob, &Asset::Token(m.gold)),
        POOL_GOLD / 4
    );
}

#[test]
fn two_tokens_trade_without_stein() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let alice = n.alice.address();
    let (x, y) = (Address::token(&alice, 0), Address::token(&alice, 1));
    let (first, second) = if x < y { (x, y) } else { (y, x) };
    let (first, second) = (Asset::Token(first), Asset::Token(second));
    let txs = vec![
        act(&n.alice, issue("AAA", 10_000_000), 0, true),
        act(&n.alice, issue("BBB", 10_000_000), 1, false),
        act(&n.alice, add(first, 2_000_000, second, 8_000_000), 2, false),
        act(&n.alice, swap(second, 1_000, first, 0), 3, false),
    ];
    let (_, rejected) = produce(&mut state, &n, txs);
    assert!(rejected.is_empty(), "{rejected:?}");
    let pool = pool_address(second, first);
    let bought = swap_output(8_000_000, 2_000_000, 1_000).expect("priced");
    assert_eq!(bought, 249);
    assert_eq!(
        state.last_outcomes()[3],
        Outcome::Swapped { amount_out: bought }
    );
    assert_eq!(state.balance_of(&pool, &first), 2_000_000 - bought);
    assert_eq!(state.balance_of(&pool, &second), 8_001_000);
    // The pool never held STEIN: it has no account at all.
    assert!(state.account(&pool).is_none());
    assert_conserved(&state, &n.genesis);
}

#[test]
fn funds_sent_straight_to_a_pool_join_its_reserves() {
    let mut m = market();
    let gold = Asset::Token(m.gold);
    // A plain transfer to the pool's address is a credit like any other
    // (SHARD-READY I-1): it cannot be refused, and it moves the price.
    let tx = transfer(&m.n.alice, m.pool, 100 * ONE_STEIN, 3, false);
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(m.state.balance_of(&m.pool, &STEIN), 2 * POOL_STEIN);
    assert_eq!(
        swap_output(2 * POOL_STEIN, POOL_GOLD, 10 * ONE_STEIN),
        Some(18_993)
    );
    let tx = act(&m.n.bob, swap(STEIN, 10 * ONE_STEIN, gold, 0), 0, true);
    let (_, rejected) = produce(&mut m.state, &m.n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(
        m.state.last_outcomes(),
        [Outcome::Swapped { amount_out: 18_993 }]
    );
    assert_conserved(&m.state, &m.n.genesis);
}

#[test]
fn a_rejected_block_leaves_no_trace_of_its_tokens_or_pools() {
    let mut m = market();
    let mut follower = m.follower();
    let gold = Asset::Token(m.gold);
    let before = (
        follower.state_root(),
        follower.tokens.clone(),
        follower.holdings.clone(),
        follower.pools.clone(),
        follower.accounts.clone(),
    );

    let txs = vec![
        act(&m.n.alice, issue("LEAD", 99), 3, false),
        act(&m.n.bob, swap(STEIN, 10 * ONE_STEIN, gold, 0), 0, true),
        act(&m.n.alice, add(STEIN, ONE_STEIN, gold, 4_000), 4, false),
    ];
    let (mut block, rejected) = produce(&mut m.state, &m.n, txs);
    assert!(rejected.is_empty(), "{rejected:?}");

    // The proposer claims a state root the transactions do not produce.
    block.header.state_root = Hash([0xaa; 32]);
    block.proposer_signature =
        m.n.validator
            .sign(Domain::Proposal, &block.header.encode())
            .expect("sign");
    assert_eq!(follower.import_block(&block), Err(BlockError::StateRoot));
    assert_eq!(
        (
            follower.state_root(),
            follower.tokens.clone(),
            follower.holdings.clone(),
            follower.pools.clone(),
            follower.accounts.clone(),
        ),
        before
    );
    assert_eq!(follower.compute_root(), before.0);
}

#[test]
fn everything_is_conserved_across_a_long_mixed_history() {
    let mut m = market();
    let gold = Asset::Token(m.gold);
    let mut follower = m.follower();
    // A fixed pseudo-random sequence: the same history on every run.
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |bound: u128| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        u128::from(x) % bound
    };
    let (mut alice_nonce, mut bob_nonce) = (3u64, 0u64);
    let mut refunded = 0;
    for round in 0..60 {
        let by_alice = next(2) == 0;
        let (who, nonce) = if by_alice {
            (&m.n.alice, &mut alice_nonce)
        } else {
            (&m.n.bob, &mut bob_nonce)
        };
        let me = who.address();
        let action = match next(6) {
            0 => swap(STEIN, 1 + next(3 * ONE_STEIN), gold, next(20_000)),
            1 => swap(
                gold,
                1 + next(1 + m.state.balance_of(&me, &gold)),
                STEIN,
                next(ONE_STEIN),
            ),
            2 => add(STEIN, 1 + next(2 * ONE_STEIN), gold, 1 + next(9_000)),
            3 => Action::RemoveLiquidity {
                pool: m.pool,
                shares: 1 + next(1 + m.state.balance_of(&me, &Asset::Token(m.pool))),
                min_a: next(ONE_STEIN / 2),
                min_b: 0,
            },
            4 => Action::TransferToken {
                token: m.gold,
                to: if by_alice {
                    m.n.bob.address()
                } else {
                    m.n.alice.address()
                },
                amount: 1 + next(1 + m.state.balance_of(&me, &gold)),
            },
            _ => Action::Transfer {
                to: m.pool,
                amount: 1 + next(1_000),
            },
        };
        let register = !by_alice && *nonce == 0;
        let tx = act(who, action, *nonce, register);
        let (block, rejected) = produce(&mut m.state, &m.n, vec![tx]);
        if rejected.is_empty() {
            *nonce += 1;
            if matches!(m.state.last_outcomes(), [Outcome::Refunded { .. }]) {
                refunded += 1;
            }
        }
        follower.import_block(&block).expect("import");
        assert_eq!(follower.state_root(), m.state.state_root(), "round {round}");
        assert_conserved(&m.state, &m.n.genesis);
        // The pool never runs dry.
        assert!(m.state.balance_of(&m.pool, &STEIN) > 0);
        assert!(m.state.balance_of(&m.pool, &gold) > 0);
    }
    // The history exercised both ways a pool operation can end.
    assert!(refunded > 0 && refunded < 60, "{refunded} refunded");
    assert!(alice_nonce + bob_nonce > 40);
}
