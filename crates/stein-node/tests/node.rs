//! The node end to end, without HTTP: submit, produce, restart, replay.

#![allow(clippy::arithmetic_side_effects)]

use std::path::Path;

use stein_crypto::{Scheme, SecretKey, create_seed_file};
use stein_node::{Node, files};
use stein_types::{
    Action, Allocation, ChainParams, Genesis, GenesisValidator, ONE_STEIN, Transaction, TxBody,
};

fn setup(dir: &Path) -> SecretKey {
    let validator =
        create_seed_file(&dir.join(files::VALIDATOR_SEED), Scheme::MlDsa44).expect("validator");
    let alice = SecretKey::from_seed(Scheme::MlDsa44, &[42; 32]).expect("alice");
    let genesis = Genesis {
        chain_id: 9,
        genesis_time_ms: 1,
        params: ChainParams::default(),
        allocations: vec![Allocation {
            address: alice.address(),
            amount: 100 * ONE_STEIN,
        }],
        validators: vec![GenesisValidator {
            public_key: validator.public_key().clone(),
            power: 1,
        }],
    };
    std::fs::write(
        dir.join(files::GENESIS),
        serde_json::to_string(&genesis).expect("json"),
    )
    .expect("write");
    alice
}

fn pay(alice: &SecretKey, nonce: u64) -> Transaction {
    let body = TxBody {
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
    Transaction::sign(body, alice, nonce == 0).expect("sign")
}

#[test]
fn transactions_survive_a_restart_and_the_log_is_reverified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alice = setup(dir.path());

    let node = Node::open(dir.path()).expect("open");
    // Nonce 1 cannot be verified before the key-registering nonce 0 is known…
    assert!(node.submit(pay(&alice, 1)).is_err());
    // …but can be once nonce 0 is queued, before any block includes it.
    node.submit(pay(&alice, 0)).expect("submit");
    node.submit(pay(&alice, 1))
        .expect("follow-up verified against the queued key");
    assert!(node.submit(pay(&alice, 0)).is_err(), "nonce already queued");
    assert_eq!(node.produce_block().expect("produce"), Some(1));
    node.submit(pay(&alice, 2)).expect("submit");
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
        reopened.submit(pay(&alice, 1)).is_err(),
        "replay after restart"
    );
}

#[test]
fn a_tampered_block_log_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alice = setup(dir.path());
    let node = Node::open(dir.path()).expect("open");
    node.submit(pay(&alice, 0)).expect("submit");
    node.produce_block().expect("produce");
    drop(node);

    // Flip one byte inside the first transaction body's amount field region.
    let path = dir.path().join(files::BLOCK_LOG);
    let mut bytes = std::fs::read(&path).expect("read");
    // log length prefix, header, tagged ML-DSA-44 proposer signature, body count
    let header_and_sig = 4 + 8 + 8 + 32 + 8 + 32 + 4 + 32 * 3 + 1 + 2420 + 4;
    bytes[header_and_sig + 50] ^= 0x01;
    std::fs::write(&path, bytes).expect("write");
    assert!(Node::open(dir.path()).is_err());
}
