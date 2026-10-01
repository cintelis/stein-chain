#![allow(clippy::arithmetic_side_effects)]

use super::*;
use stein_crypto::Signature;
use stein_types::{Allocation, GenesisValidator, ONE_STEIN};

const CHAIN: ChainId = 31337;

struct Net {
    validator: SecretKey,
    alice: SecretKey,
    bob: SecretKey,
    genesis: Genesis,
}

fn key(n: u8) -> SecretKey {
    SecretKey::from_seed(stein_crypto::Scheme::MlDsa44, &[n; 32]).expect("key")
}

fn net() -> Net {
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

fn transfer(
    from: &SecretKey,
    to: Address,
    amount: Amount,
    nonce: u64,
    register: bool,
) -> Transaction {
    let body = TxBody {
        chain_id: CHAIN,
        sender: from.address(),
        nonce,
        fee: ONE_STEIN / 100,
        expires_at: 100,
        action: Action::Transfer { to, amount },
    };
    Transaction::sign(body, from, register).expect("sign")
}

fn produce(state: &mut State, n: &Net, txs: Vec<Transaction>) -> (Block, Vec<(Hash, TxError)>) {
    let ts = 2_000 + state.height() * 1_000;
    state.build_block(txs, &n.validator, ts).expect("build")
}

#[test]
fn a_transfer_moves_funds_pays_the_proposer_and_registers_the_key() {
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
    assert_eq!(
        state
            .account(&n.validator.address())
            .expect("validator")
            .balance,
        fee
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
    let total: Amount = [&n.alice, &n.bob, &n.validator]
        .iter()
        .map(|k| state.account(&k.address()).map_or(0, |a| a.balance))
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
