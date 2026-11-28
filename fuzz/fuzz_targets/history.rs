//! A history of blocks of transactions, valid and not, signed here on the
//! fuzzer's behalf: the state machine, deep.
//!
//! The fuzzer cannot forge a signature, so it describes transactions and this
//! target signs them, as three accounts and as the validator. After every
//! block:
//!
//! * no STEIN was created or lost;
//! * a follower that verifies the block reaches the leader's state root;
//! * a block with a refused transaction slipped in, signed by the validator,
//!   is refused and leaves the follower as it was.
#![no_main]

use std::sync::LazyLock;

use borsh::BorshDeserialize;
use libfuzzer_sys::fuzz_target;
use stein_crypto::{Address, Domain, Scheme, SecretKey};
use stein_state::State;
use stein_types::{
    Action, Allocation, Amount, Asset, Block, BlockHeader, ChainParams, Genesis, GenesisValidator,
    ONE_STEIN, Transaction, TxBody,
};

const CHAIN: u64 = 31_337;
const ACTORS: usize = 3;
const MAX_BLOCKS: usize = 6;
const MAX_STEPS: usize = 5;

struct World {
    validator: SecretKey,
    actors: Vec<SecretKey>,
    genesis: Genesis,
}

static WORLD: LazyLock<World> = LazyLock::new(|| {
    let key = |n: u8| SecretKey::from_seed(Scheme::MlDsa44, &[n; 32]).expect("key");
    let validator = key(1);
    let actors = vec![key(2), key(3), key(4)];
    let genesis = Genesis {
        chain_id: CHAIN,
        genesis_time_ms: 1_000,
        params: ChainParams::default(),
        allocations: vec![
            Allocation {
                address: actors[0].address(),
                amount: 1_000 * ONE_STEIN,
            },
            Allocation {
                address: actors[1].address(),
                amount: 300 * ONE_STEIN,
            },
        ],
        validators: vec![GenesisValidator {
            public_key: validator.public_key().clone(),
            power: 1,
        }],
    };
    World {
        validator,
        actors,
        genesis,
    }
});

/// One transaction, as the fuzzer describes it.
#[derive(BorshDeserialize)]
struct Step {
    who: u8,
    /// Rarely non-zero: a wrong nonce, fee or key flag ends a sender's block.
    mischief: u8,
    action: Action,
}

/// Tokens and pools that exist.
fn known(state: &State) -> Vec<Address> {
    state
        .tokens()
        .map(|(address, _)| *address)
        .chain(state.pools().map(|(address, _)| *address))
        .collect()
}

/// The fuzzer cannot guess an address that is a hash, so most of the
/// addresses it writes are read as "the n-th thing that exists".
fn token(state: &State, written: Address) -> Address {
    let known = known(state);
    match written.0[0] {
        n if n < 0xf0 && !known.is_empty() => known[usize::from(n) % known.len()],
        _ => written,
    }
}

fn asset(state: &State, written: Asset) -> Asset {
    match written {
        Asset::Stein => Asset::Stein,
        Asset::Token(address) => Asset::Token(token(state, address)),
    }
}

fn recipient(state: &State, written: Address) -> Address {
    match written.0[0] {
        n if usize::from(n) < ACTORS => WORLD.actors[usize::from(n)].address(),
        n if n < 0x80 => token(state, written),
        _ => written,
    }
}

/// Amounts with the top bit clear are read against what the sender holds, so
/// that most are affordable; with it set, as written.
fn amount(written: Amount, balance: Amount) -> Amount {
    if written >> 127 == 1 {
        written
    } else {
        written % balance.saturating_add(2)
    }
}

fn resolve(state: &State, me: &Address, action: Action) -> Action {
    let held = |asset: &Asset| state.balance_of(me, asset);
    match action {
        Action::Transfer { to, amount: a } => Action::Transfer {
            to: recipient(state, to),
            amount: amount(a, held(&Asset::Stein)),
        },
        issue @ Action::IssueToken { .. } => issue,
        Action::TransferToken {
            token: t,
            to,
            amount: a,
        } => {
            let t = token(state, t);
            Action::TransferToken {
                token: t,
                to: recipient(state, to),
                amount: amount(a, held(&Asset::Token(t))),
            }
        }
        Action::AddLiquidity {
            asset_a,
            amount_a,
            asset_b,
            amount_b,
            min_shares,
        } => {
            let (asset_a, asset_b) = (asset(state, asset_a), asset(state, asset_b));
            Action::AddLiquidity {
                asset_a,
                amount_a: amount(amount_a, held(&asset_a)),
                asset_b,
                amount_b: amount(amount_b, held(&asset_b)),
                min_shares,
            }
        }
        Action::RemoveLiquidity {
            pool,
            shares,
            min_a,
            min_b,
        } => {
            let pool = token(state, pool);
            Action::RemoveLiquidity {
                pool,
                shares: amount(shares, held(&Asset::Token(pool))),
                min_a,
                min_b,
            }
        }
        Action::Swap {
            asset_in,
            amount_in,
            asset_out,
            min_out,
        } => {
            let asset_in = asset(state, asset_in);
            Action::Swap {
                asset_in,
                amount_in: amount(amount_in, held(&asset_in)),
                asset_out: asset(state, asset_out),
                min_out,
            }
        }
    }
}

fn sign(state: &State, steps: Vec<Step>) -> Vec<Transaction> {
    let mut queued = [0u64; ACTORS];
    steps
        .into_iter()
        .take(MAX_STEPS)
        .map(|step| {
            let i = usize::from(step.who) % ACTORS;
            let who = &WORLD.actors[i];
            let me = who.address();
            let account = state.account(&me);
            let (skew, fee, flip_key) = match step.mischief {
                0xff => (1, ONE_STEIN / 100, false),
                0xfe => (0, 0, false),
                0xfd => (0, Amount::MAX, false),
                0xfc => (0, ONE_STEIN / 100, true),
                _ => (0, ONE_STEIN / 100, false),
            };
            let nonce = account
                .map_or(0, |a| a.nonce)
                .saturating_add(queued[i])
                .saturating_add(skew);
            let registered = account.is_some_and(|a| a.public_key.is_some());
            let register = (!registered && queued[i] == 0) != flip_key;
            queued[i] = queued[i].saturating_add(1);
            let body = TxBody {
                version: TxBody::VERSION,
                chain_id: CHAIN,
                sender: me,
                nonce,
                fee,
                expires_at: 100,
                action: resolve(state, &me, step.action),
            };
            Transaction::sign(body, who, register).expect("sign")
        })
        .collect()
}

/// `block` with `extra` appended and the roots that go with it, signed by the
/// validator. The state root is the honest block's.
fn with_extra(block: &Block, extra: Transaction) -> Block {
    let mut bodies = block.bodies.clone();
    let mut witnesses = block.witnesses.clone().expect("witnesses");
    bodies.push(extra.body);
    witnesses.push(extra.witness);
    let header = BlockHeader {
        tx_count: bodies.len() as u32,
        tx_root: Block::tx_root(&bodies),
        witness_root: Block::witness_root(&witnesses),
        ..block.header.clone()
    };
    let proposer_signature = WORLD
        .validator
        .sign(Domain::Proposal, &header.encode())
        .expect("sign");
    Block {
        header,
        proposer_signature,
        bodies,
        witnesses: Some(witnesses),
    }
}

fn stein_in_existence(state: &State) -> Amount {
    state
        .richest(usize::MAX)
        .iter()
        .map(|(_, account)| account.balance)
        .sum()
}

fuzz_target!(|data: &[u8]| {
    let Ok(history) = borsh::from_slice::<Vec<Vec<Step>>>(data) else {
        return;
    };
    let supply = WORLD.genesis.total_supply().expect("supply");
    let mut leader = State::from_genesis(&WORLD.genesis).expect("genesis");
    let mut follower = State::from_genesis(&WORLD.genesis).expect("genesis");

    for steps in history.into_iter().take(MAX_BLOCKS) {
        let txs = sign(&leader, steps);
        let timestamp = 2_000 + leader.height() * 1_000;
        let (block, rejected) = leader
            .build_block(txs.clone(), &WORLD.validator, timestamp)
            .expect("build");
        assert_eq!(block.bodies.len() + rejected.len(), txs.len());
        assert_eq!(stein_in_existence(&leader), supply);

        for tx in txs
            .iter()
            .filter(|tx| rejected.iter().any(|(id, _)| *id == tx.id()))
        {
            let before = (follower.state_root(), follower.height(), follower.head());
            assert!(follower.import_block(&with_extra(&block, tx.clone())).is_err());
            assert_eq!(
                (follower.state_root(), follower.height(), follower.head()),
                before
            );
        }

        follower.import_block(&block).expect("the honest block");
        assert_eq!(follower.state_root(), leader.state_root());
        assert_eq!(stein_in_existence(&follower), supply);
    }
});
