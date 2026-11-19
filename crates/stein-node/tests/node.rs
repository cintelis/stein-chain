//! The node end to end, without HTTP: submit, produce, restart, replay.

#![allow(clippy::arithmetic_side_effects)]

use std::path::Path;

use stein_crypto::{Scheme, SecretKey, create_seed_file};
use stein_node::{Node, files};
use stein_types::{
    Action, Allocation, ChainParams, Genesis, GenesisValidator, ONE_STEIN, Transaction, TxBody,
};

fn setup(dir: &Path) -> SecretKey {
    setup_with(dir, &[])
}

/// A devnet whose genesis lists this node's validator and `others`.
fn setup_with(dir: &Path, others: &[&SecretKey]) -> SecretKey {
    let validator =
        create_seed_file(&dir.join(files::VALIDATOR_SEED), Scheme::MlDsa44).expect("validator");
    let alice = SecretKey::from_seed(Scheme::MlDsa44, &[42; 32]).expect("alice");
    let validators = std::iter::once(&validator)
        .chain(others.iter().copied())
        .map(|key| GenesisValidator {
            public_key: key.public_key().clone(),
            power: 1,
        })
        .collect();
    let genesis = Genesis {
        chain_id: 9,
        genesis_time_ms: 1,
        params: ChainParams::default(),
        allocations: vec![Allocation {
            address: alice.address(),
            amount: 100 * ONE_STEIN,
        }],
        validators,
    };
    std::fs::write(
        dir.join(files::GENESIS),
        serde_json::to_string(&genesis).expect("json"),
    )
    .expect("write");
    alice
}

fn pay(alice: &SecretKey, nonce: u64) -> Transaction {
    pay_with(alice, nonce, nonce == 0)
}

fn pay_with(alice: &SecretKey, nonce: u64, register: bool) -> Transaction {
    let body = TxBody {
        version: TxBody::VERSION,
        chain_id: 9,
        sender: alice.address(),
        nonce,
        fee: ONE_STEIN / 100,
        expires_at: 50,
        action: Action::Transfer {
            to: stein_crypto::Address([5; 32]),
            amount: ONE_STEIN,
        },
    };
    Transaction::sign(body, alice, register).expect("sign")
}

#[tokio::test(flavor = "multi_thread")]
async fn transactions_that_could_never_be_included_are_not_queued() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alice = setup(dir.path());
    let node = Node::open(dir.path()).expect("open");

    // A validly signed transaction from an account that holds nothing.
    let nobody = SecretKey::from_seed(Scheme::MlDsa44, &[77; 32]).expect("key");
    let error = node.submit(pay(&nobody, 0)).await.expect_err("unfunded");
    assert!(error.contains("insufficient balance"), "{error}");

    // A funded account, but a nonce that leaves a gap: with the key attached
    // it verifies, yet no block could include it before nonce 0.
    let error = node
        .submit(pay_with(&alice, 1, true))
        .await
        .expect_err("gap");
    assert!(error.contains("leaves a gap"), "{error}");
    node.submit(pay(&alice, 0)).await.expect("nonce 0");
    let error = node.submit(pay(&alice, 2)).await.expect_err("gap");
    assert!(error.contains("leaves a gap"), "{error}");
    node.submit(pay(&alice, 1)).await.expect("nonce 1");
    node.submit(pay(&alice, 2)).await.expect("nonce 2");

    assert_eq!(node.mempool.lock().len(), 3);
    assert_eq!(node.produce_block().expect("produce"), Some(1));
    assert!(node.mempool.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_validator_produces_only_at_the_heights_it_is_scheduled_for() {
    let dir = tempfile::tempdir().expect("tempdir");
    let other = SecretKey::from_seed(Scheme::MlDsa44, &[43; 32]).expect("other");
    let alice = setup_with(dir.path(), &[&other]);
    let me = stein_crypto::secret_key_from_seed_file(&dir.path().join(files::VALIDATOR_SEED))
        .expect("seed")
        .address();

    let node = Node::open(dir.path()).expect("open");
    // Which of the two proposes first depends on their addresses.
    let first = node.state.read().proposer_for(1).key.address() == me;
    node.submit(pay(&alice, 0)).await.expect("submit");
    assert_eq!(
        node.produce_block().expect("produce"),
        first.then_some(1),
        "produces height 1 only if scheduled for it"
    );
    // Either way the next height is now the other validator's.
    if first {
        node.submit(pay(&alice, 1)).await.expect("submit");
    }
    assert_eq!(node.produce_block().expect("produce"), None);
    assert_eq!(node.state.read().height(), u64::from(first));
    drop(node);

    // Nothing it wrote is refused when it replays its own log.
    let reopened = Node::open(dir.path()).expect("reopen");
    assert_eq!(reopened.state.read().height(), u64::from(first));
}

#[tokio::test(flavor = "multi_thread")]
async fn transactions_survive_a_restart_and_the_log_is_reverified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alice = setup(dir.path());

    let node = Node::open(dir.path()).expect("open");
    // Nonce 1 cannot be verified before the key-registering nonce 0 is known…
    assert!(node.submit(pay(&alice, 1)).await.is_err());
    // …but can be once nonce 0 is queued, before any block includes it.
    node.submit(pay(&alice, 0)).await.expect("submit");
    node.submit(pay(&alice, 1))
        .await
        .expect("follow-up verified against the queued key");
    assert!(
        node.submit(pay(&alice, 0)).await.is_err(),
        "nonce already queued"
    );
    assert_eq!(node.produce_block().expect("produce"), Some(1));
    node.submit(pay(&alice, 2)).await.expect("submit");
    assert_eq!(node.produce_block().expect("produce"), Some(2));
    assert_eq!(
        node.produce_block().expect("produce"),
        None,
        "no empty blocks"
    );
    let root = node.state.read().state_root();
    let id = pay(&alice, 2).id();
    assert_eq!(node.store.read().locate(&id).map(|l| l.height), Some(2));
    drop(node);

    let reopened = Node::open(dir.path()).expect("reopen");
    assert_eq!(reopened.state.read().height(), 2);
    assert_eq!(reopened.state.read().state_root(), root);
    assert_eq!(
        reopened
            .state
            .read()
            .account(&alice.address())
            .expect("alice")
            .nonce,
        3
    );
    assert!(
        reopened.submit(pay(&alice, 1)).await.is_err(),
        "replay after restart"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tampered_block_log_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alice = setup(dir.path());
    let node = Node::open(dir.path()).expect("open");
    node.submit(pay(&alice, 0)).await.expect("submit");
    node.produce_block().expect("produce");
    drop(node);

    // Flip one byte inside the first transaction body's amount field region.
    let path = dir.path().join(files::BLOCK_LOG);
    let mut bytes = std::fs::read(&path).expect("read");
    // log length prefix, header (with version), tagged ML-DSA-44 proposer signature, body count
    let header_and_sig = 4 + 1 + 8 + 8 + 32 + 8 + 32 + 4 + 32 * 3 + 1 + 2420 + 4;
    bytes[header_and_sig + 50] ^= 0x01;
    std::fs::write(&path, bytes).expect("write");
    assert!(Node::open(dir.path()).is_err());
}
