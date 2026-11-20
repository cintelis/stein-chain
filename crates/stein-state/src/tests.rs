#![allow(clippy::arithmetic_side_effects)]

use super::*;
use stein_crypto::Signature;
use stein_types::{Allocation, GenesisValidator, ONE_STEIN};

pub(crate) const CHAIN: ChainId = 31337;

pub(crate) struct Net {
    pub(crate) validator: SecretKey,
    pub(crate) alice: SecretKey,
    pub(crate) bob: SecretKey,
    pub(crate) genesis: Genesis,
}

pub(crate) fn key(n: u8) -> SecretKey {
    SecretKey::from_seed(stein_crypto::Scheme::MlDsa44, &[n; 32]).expect("key")
}

pub(crate) fn net() -> Net {
    let (validator, alice, bob) = (key(1), key(2), key(3));
    let genesis = Genesis {
        chain_id: CHAIN,
        genesis_time_ms: 1_000,
        params: ChainParams::default(),
        allocations: vec![Allocation {
            address: alice.address(),
            amount: 1_000 * ONE_STEIN,
        }],
        validators: vec![GenesisValidator {
            public_key: validator.public_key().clone(),
            power: 1,
        }],
    };
    Net {
        validator,
        alice,
        bob,
        genesis,
    }
}

pub(crate) fn transfer(
    from: &SecretKey,
    to: Address,
    amount: Amount,
    nonce: u64,
    register: bool,
) -> Transaction {
    let body = TxBody {
        version: TxBody::VERSION,
        chain_id: CHAIN,
        sender: from.address(),
        nonce,
        fee: ONE_STEIN / 100,
        expires_at: 100,
        action: Action::Transfer { to, amount },
    };
    Transaction::sign(body, from, register).expect("sign")
}

pub(crate) fn produce(
    state: &mut State,
    n: &Net,
    txs: Vec<Transaction>,
) -> (Block, Vec<(Hash, TxError)>) {
    let ts = 2_000 + state.height() * 1_000;
    state.build_block(txs, &n.validator, ts).expect("build")
}

#[test]
fn a_transfer_moves_funds_pays_the_fee_pool_and_registers_the_key() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let tx = transfer(&n.alice, n.bob.address(), 10 * ONE_STEIN, 0, true);
    let (block, rejected) = produce(&mut state, &n, vec![tx]);
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(block.bodies.len(), 1);

    let fee = ONE_STEIN / 100;
    let alice = state.account(&n.alice.address()).expect("alice");
    assert_eq!(alice.balance, 990 * ONE_STEIN - fee);
    assert_eq!(alice.nonce, 1);
    assert_eq!(alice.public_key.as_ref(), Some(n.alice.public_key()));
    assert_eq!(
        state.account(&n.bob.address()).expect("bob").balance,
        10 * ONE_STEIN
    );
    // Fees collect in the fee pool, not with the proposer (SHARD-READY I-6).
    assert_eq!(
        state
            .account(&stein_types::fee_pool())
            .expect("pool")
            .balance,
        fee
    );
    assert!(state.account(&n.validator.address()).is_none());
}

#[test]
fn the_fee_pool_pays_validators_at_each_epoch_boundary() {
    let mut n = net();
    let second = key(8);
    n.genesis.params.epoch_blocks = 2;
    n.genesis.validators.push(GenesisValidator {
        public_key: second.public_key().clone(),
        power: 3,
    });
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let fee = ONE_STEIN / 100;

    // Proposers alternate by height; each signs the block it proposes.
    for height in 1..=4u64 {
        let nonce = height - 1;
        let proposer = if leader.proposer_for(height).key.address() == n.validator.address() {
            &n.validator
        } else {
            &second
        };
        let tx = transfer(&n.alice, n.bob.address(), ONE_STEIN, nonce, nonce == 0);
        let (block, rejected) = leader
            .build_block(vec![tx], proposer, 10_000 + height * 1_000)
            .expect("build");
        assert!(rejected.is_empty(), "{rejected:?}");
        follower.import_block(&block).expect("import");
        assert_eq!(follower.state_root(), leader.state_root());

        let pool = leader
            .account(&stein_types::fee_pool())
            .map_or(0, |a| a.balance);
        if height % 2 == 1 {
            assert_eq!(pool, fee, "mid-epoch: the fee waits in the pool");
        } else {
            // Paid out 1:3 by power: 0.02 STEIN in the pool after two blocks.
            assert_eq!(pool, 0, "epoch boundary at height {height}: pool emptied");
        }
    }
    let one = leader
        .account(&n.validator.address())
        .map_or(0, |a| a.balance);
    let three = leader.account(&second.address()).map_or(0, |a| a.balance);
    assert_eq!((one, three), (fee, 3 * fee), "four fees split 1:3");
}

/// A chain's history from before tokens must replay to the roots its blocks
/// carry: while no token or pool exists, the state root is exactly what it
/// was. These roots were computed by the code as it stood before tokens.
#[test]
fn state_roots_from_before_tokens_are_unchanged() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let mut roots = vec![state.state_root().to_string()];
    for tx in [
        transfer(&n.alice, n.bob.address(), 10 * ONE_STEIN, 0, true),
        transfer(&n.bob, n.alice.address(), 3 * ONE_STEIN, 0, true),
    ] {
        let (_, rejected) = produce(&mut state, &n, vec![tx]);
        assert!(rejected.is_empty(), "{rejected:?}");
        roots.push(state.state_root().to_string());
    }
    assert_eq!(
        roots,
        [
            "954046e0063b774aade70e7963f1fbff1068ceb9c2aaf110a6198537859b621d",
            "0866cd41fc94be08986e2d26824ca5c0043af355b8c80dc0ddd98f1a941fe3e3",
            "422d83a7f7529ab76a4d9318667085cadf1a57cfcfba5de4ffe478b3d3c6734c",
        ]
    );
}

#[test]
fn genesis_rejects_a_supply_that_could_overflow_a_credit() {
    let mut n = net();
    n.genesis.allocations[0].amount = stein_types::MAX_TOTAL_SUPPLY;
    assert!(State::from_genesis(&n.genesis).is_ok());
    n.genesis.allocations[0].amount = stein_types::MAX_TOTAL_SUPPLY + 1;
    assert!(matches!(
        State::from_genesis(&n.genesis),
        Err(GenesisError::SupplyTooLarge)
    ));
}

#[test]
fn unknown_formats_are_refused() {
    let n = net();
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let mut future = transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true);
    future.body.version = 2;
    assert_eq!(
        leader.check_transaction(&future, None),
        Err(TxError::UnsupportedVersion(2))
    );

    let (mut block, _) = produce(&mut leader, &n, vec![]);
    block.header.version = 2;
    block.proposer_signature = n
        .validator
        .sign(Domain::Proposal, &block.header.encode())
        .expect("sign");
    assert_eq!(
        follower.import_block(&block),
        Err(BlockError::UnsupportedVersion(2))
    );
}

#[test]
fn every_validator_reaches_the_same_state() {
    let n = net();
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let txs = vec![
        transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true),
        // Second tx from the same new account in the same block: must use the
        // key registered by the first, not attach it again.
        transfer(&n.alice, n.bob.address(), ONE_STEIN, 1, false),
    ];
    let (block, rejected) = produce(&mut leader, &n, txs);
    assert!(rejected.is_empty(), "{rejected:?}");
    follower.import_block(&block).expect("import");
    assert_eq!(follower.state_root(), leader.state_root());
    assert_eq!(follower.head(), leader.head());

    let (block2, _) = produce(
        &mut leader,
        &n,
        vec![transfer(&n.alice, n.bob.address(), ONE_STEIN, 2, false)],
    );
    follower.import_block(&block2).expect("import 2");
    assert_eq!(follower.state_root(), leader.state_root());
}

#[test]
fn replay_and_bad_transactions_are_left_out() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    produce(
        &mut state,
        &n,
        vec![transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true)],
    );

    let mut wrong_chain = transfer(&n.alice, n.bob.address(), ONE_STEIN, 1, false);
    wrong_chain.body.chain_id = 1;
    let candidates = vec![
        transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, false), // replay: nonce 0 again
        transfer(&n.alice, n.bob.address(), ONE_STEIN, 1, true),  // key already registered
        transfer(&n.bob, n.alice.address(), ONE_STEIN, 0, false), // bob has no key registered
        transfer(&n.alice, n.bob.address(), 10_000 * ONE_STEIN, 1, false), // insufficient balance
        wrong_chain,                                              // wrong chain
    ];
    let (block, rejected) = produce(&mut state, &n, candidates);
    assert!(block.bodies.is_empty());
    let reasons: Vec<&TxError> = rejected.iter().map(|(_, e)| e).collect();
    assert!(
        reasons
            .iter()
            .any(|e| matches!(e, TxError::WrongChain { .. })),
        "{reasons:?}"
    );
    assert!(reasons.contains(&&TxError::KeyAlreadyRegistered));
    assert!(reasons.contains(&&TxError::KeyMissing));
    assert!(reasons.contains(&&TxError::InsufficientBalance));
    assert!(reasons.iter().any(|e| matches!(
        e,
        TxError::BadNonce {
            expected: 1,
            got: 0
        }
    )));
}

#[test]
fn a_transaction_that_does_not_fit_waits_with_its_successors() {
    let mut n = net();
    // Room for the key-registering transfer (~3.9 KB) or two plain ones
    // (~2.5 KB each), never for all three.
    n.genesis.params.max_block_bytes = 5_500;
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let txs: Vec<Transaction> = (0..3)
        .map(|nonce| transfer(&n.alice, n.bob.address(), ONE_STEIN, nonce, nonce == 0))
        .collect();

    // Nonce 1 does not fit behind nonce 0. It and nonce 2 are left for the
    // next block, not rejected: a rejection would drop them from the mempool.
    let (block, rejected) = produce(&mut state, &n, txs.clone());
    assert_eq!(block.bodies.len(), 1);
    assert_eq!(rejected, vec![]);

    let (block, rejected) = produce(&mut state, &n, txs[1..].to_vec());
    assert_eq!(block.bodies.len(), 2);
    assert_eq!(rejected, vec![]);
    assert_eq!(state.account(&n.alice.address()).expect("alice").nonce, 3);
}

#[test]
fn a_forged_signature_is_left_out_by_the_leader() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let mut tx = transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true);
    // Mallory signs alice's body with her own key but attaches alice's key.
    let mallory = key(9);
    tx.witness.signature = mallory
        .sign(Domain::Transaction, &tx.body.encode())
        .expect("sign");
    let (block, rejected) = produce(&mut state, &n, vec![tx]);
    assert!(block.bodies.is_empty());
    assert_eq!(rejected[0].1, TxError::BadSignature);
}

/// A malicious leader includes a forged transaction and signs the block
/// anyway. Followers must reject it, not trust the leader's verification.
#[test]
fn followers_reject_a_block_with_a_forged_transaction() {
    let n = net();
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let (mut block, _) = produce(
        &mut leader,
        &n,
        vec![transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true)],
    );

    let mallory = key(9);
    let witnesses = block.witnesses.as_mut().expect("witnesses");
    witnesses[0].signature = mallory
        .sign(Domain::Transaction, &block.bodies[0].encode())
        .expect("sign");
    block.header.witness_root = Block::witness_root(witnesses);
    block.proposer_signature = n
        .validator
        .sign(Domain::Proposal, &block.header.encode())
        .expect("sign");

    let before = follower.state_root();
    assert_eq!(
        follower.import_block(&block),
        Err(BlockError::InvalidTx {
            index: 0,
            error: TxError::BadSignature
        })
    );
    assert_eq!(follower.state_root(), before);
    assert_eq!(follower.height(), 0);
}

#[test]
fn followers_reject_blocks_without_witnesses_or_with_wrong_roots() {
    let n = net();
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let (block, _) = produce(
        &mut leader,
        &n,
        vec![transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true)],
    );

    let mut stripped = block.clone();
    stripped.prune();
    assert_eq!(
        follower.import_block(&stripped),
        Err(BlockError::MissingWitnesses)
    );

    let mut lying_root = block.clone();
    lying_root.header.state_root = Hash([7; 32]);
    lying_root.proposer_signature = n
        .validator
        .sign(Domain::Proposal, &lying_root.header.encode())
        .expect("sign");
    assert_eq!(
        follower.import_block(&lying_root),
        Err(BlockError::StateRoot)
    );
    assert_eq!(follower.height(), 0, "a rejected block must leave no trace");

    let mut unsigned = block.clone();
    unsigned.proposer_signature =
        Signature::from_bytes(stein_crypto::Scheme::MlDsa44, &[0u8; 2420]).expect("len");
    assert_eq!(
        follower.import_block(&unsigned),
        Err(BlockError::BadProposerSignature)
    );

    let mut impostor = block.clone();
    impostor.proposer_signature = key(9)
        .sign(Domain::Proposal, &impostor.header.encode())
        .expect("sign");
    assert_eq!(
        follower.import_block(&impostor),
        Err(BlockError::BadProposerSignature)
    );

    follower
        .import_block(&block)
        .expect("the honest block still imports");
    assert_eq!(
        follower.import_block(&block),
        Err(BlockError::WrongHeight {
            expected: 2,
            got: 1
        })
    );
}

#[test]
fn a_rejected_block_restores_new_accounts_and_keys() {
    let n = net();
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let (mut block, _) = produce(
        &mut leader,
        &n,
        vec![transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true)],
    );
    block.header.state_root = Hash([1; 32]);
    block.proposer_signature = n
        .validator
        .sign(Domain::Proposal, &block.header.encode())
        .expect("sign");
    assert_eq!(follower.import_block(&block), Err(BlockError::StateRoot));
    assert!(follower.account(&n.bob.address()).is_none());
    assert!(
        follower
            .account(&n.alice.address())
            .expect("alice")
            .public_key
            .is_none()
    );
    // Alice can still register her key afterwards.
    let tx = transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true);
    assert_eq!(follower.check_transaction(&tx, None), Ok(()));
}

#[test]
fn self_transfers_only_cost_the_fee() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    produce(
        &mut state,
        &n,
        vec![transfer(
            &n.alice,
            n.alice.address(),
            5 * ONE_STEIN,
            0,
            true,
        )],
    );
    let alice = state.account(&n.alice.address()).expect("alice");
    assert_eq!(alice.balance, 1_000 * ONE_STEIN - ONE_STEIN / 100);
}

#[test]
fn supply_is_conserved() {
    let n = net();
    let mut state = State::from_genesis(&n.genesis).expect("genesis");
    let supply = n.genesis.total_supply().expect("supply");
    let mut txs = vec![transfer(&n.alice, n.bob.address(), 3 * ONE_STEIN, 0, true)];
    txs.extend((1..20).map(|i| transfer(&n.alice, n.bob.address(), ONE_STEIN, i, false)));
    produce(&mut state, &n, txs);
    produce(
        &mut state,
        &n,
        vec![transfer(&n.bob, n.alice.address(), ONE_STEIN, 0, true)],
    );
    let total: Amount = [
        n.alice.address(),
        n.bob.address(),
        n.validator.address(),
        stein_types::fee_pool(),
    ]
    .iter()
    .map(|a| state.account(a).map_or(0, |a| a.balance))
    .sum();
    assert_eq!(total, supply);
}

#[test]
fn a_warm_signature_cache_never_vouches_for_different_bytes() {
    let n = net();
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let honest = transfer(&n.alice, n.bob.address(), ONE_STEIN, 0, true);
    // The follower saw the honest transaction in its mempool: cache is warm.
    assert_eq!(follower.check_transaction(&honest, None), Ok(()));

    let (mut block, _) = produce(&mut leader, &n, vec![honest]);
    // Same body, forged witness (a signature by another key).
    let witnesses = block.witnesses.as_mut().expect("witnesses");
    witnesses[0].signature = key(9)
        .sign(Domain::Transaction, &block.bodies[0].encode())
        .expect("sign");
    block.header.witness_root = Block::witness_root(witnesses);
    block.proposer_signature = n
        .validator
        .sign(Domain::Proposal, &block.header.encode())
        .expect("sign");
    assert_eq!(
        follower.import_block(&block),
        Err(BlockError::InvalidTx {
            index: 0,
            error: TxError::BadSignature
        })
    );
}

#[test]
fn signatures_checked_at_admission_are_not_checked_again() {
    let n = net();
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let txs: Vec<Transaction> = (0..5)
        .map(|i| transfer(&n.alice, n.bob.address(), ONE_STEIN, i, i == 0))
        .collect();
    let queued = txs[0].witness.public_key.clone();
    for tx in &txs {
        follower
            .check_transaction(tx, queued.as_ref())
            .expect("admit");
    }
    let (block, rejected) = produce(&mut leader, &n, txs);
    assert!(rejected.is_empty());
    let before = follower.sig_cache().hits();
    follower.import_block(&block).expect("import");
    assert_eq!(follower.sig_cache().hits() - before, 5);
    assert_eq!(follower.state_root(), leader.state_root());
}

#[test]
fn accounts_on_different_schemes_share_a_block() {
    let n = net();
    let carol = SecretKey::from_seed(stein_crypto::Scheme::MlDsa65, &[4; 32]).expect("key");
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");
    let (b1, _) = produce(
        &mut leader,
        &n,
        vec![transfer(&n.alice, carol.address(), 10 * ONE_STEIN, 0, true)],
    );
    let (b2, rejected) = produce(
        &mut leader,
        &n,
        vec![
            transfer(&carol, n.bob.address(), ONE_STEIN, 0, true),
            transfer(&n.alice, n.bob.address(), ONE_STEIN, 1, false),
        ],
    );
    assert!(rejected.is_empty(), "{rejected:?}");
    assert_eq!(b2.bodies.len(), 2);
    follower.import_block(&b1).expect("b1");
    follower.import_block(&b2).expect("b2");
    assert_eq!(follower.state_root(), leader.state_root());
}

/// FN-DSA-512 per the draft FIPS 206 (SPEC §3.5): accepted for accounts where
/// genesis lists it, never for validators.
#[test]
fn fn_dsa_accounts_work_where_genesis_lists_the_scheme() {
    let fn_dsa = stein_crypto::Scheme::FnDsa512Draft;
    let n = net();
    let frank = SecretKey::from_seed(fn_dsa, &[6; 32]).expect("key");

    assert!(n.genesis.params.tx_schemes.contains(&fn_dsa), "default");
    let mut leader = State::from_genesis(&n.genesis).expect("genesis");
    let mut follower = State::from_genesis(&n.genesis).expect("genesis");

    let (b1, _) = produce(
        &mut leader,
        &n,
        vec![transfer(&n.alice, frank.address(), 10 * ONE_STEIN, 0, true)],
    );
    let first = transfer(&frank, n.bob.address(), ONE_STEIN, 0, true);
    let second = transfer(&frank, n.bob.address(), ONE_STEIN, 1, false);
    // 122-byte body + option tag + tagged 897-byte key + tagged 666-byte signature.
    assert_eq!((first.encoded_len(), second.encoded_len()), (1688, 790));
    let (b2, rejected) = produce(&mut leader, &n, vec![first.clone(), second]);
    assert!(rejected.is_empty(), "{rejected:?}");
    follower.import_block(&b1).expect("b1");
    follower.import_block(&b2).expect("b2");
    assert_eq!(follower.state_root(), leader.state_root());
    assert_eq!(leader.account(&frank.address()).expect("frank").nonce, 2);

    // The same transaction on an ML-DSA-only network is refused at the door.
    let mut ml_dsa_only = n.genesis.clone();
    ml_dsa_only.params.tx_schemes.retain(|s| !s.is_draft());
    let standard = State::from_genesis(&ml_dsa_only).expect("genesis");
    assert_eq!(
        standard.check_transaction(&first, None),
        Err(TxError::UnsupportedScheme)
    );

    // And no genesis may give validators a draft-standard scheme.
    let mut bad = n.genesis.clone();
    bad.params.validator_schemes.push(fn_dsa);
    assert!(matches!(
        State::from_genesis(&bad),
        Err(GenesisError::DraftValidatorScheme(_))
    ));
}
