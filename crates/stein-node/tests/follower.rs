//! Two nodes, node to node: a validator served over real HTTP, and a follower
//! that forwards transactions to it and re-verifies its blocks.

#![allow(clippy::arithmetic_side_effects)]

use std::path::Path;
use std::sync::Arc;

use stein_crypto::{Address, Hash, Scheme, SecretKey, create_seed_file};
use stein_node::{Node, NodeError, files, rpc};
use stein_types::{
    Action, Allocation, ChainParams, Genesis, GenesisValidator, ONE_STEIN, Transaction, TxBody,
};

fn setup_validator(dir: &Path, chain_id: u64) -> (SecretKey, Genesis) {
    let validator =
        create_seed_file(&dir.join(files::VALIDATOR_SEED), Scheme::MlDsa44).expect("validator");
    let alice = SecretKey::from_seed(Scheme::MlDsa44, &[42; 32]).expect("alice");
    let genesis = Genesis {
        chain_id,
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
    (alice, genesis)
}

fn pay(alice: &SecretKey, chain_id: u64, nonce: u64) -> Transaction {
    let body = TxBody {
        version: TxBody::VERSION,
        chain_id,
        sender: alice.address(),
        nonce,
        fee: ONE_STEIN / 100,
        expires_at: 50,
        action: Action::Transfer {
            to: Address([5; 32]),
            amount: ONE_STEIN,
        },
    };
    Transaction::sign(body, alice, nonce == 0).expect("sign")
}

/// Serves `node`'s RPC on an ephemeral localhost port; returns its base URL.
async fn serve(node: Arc<Node>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, rpc::router(node)).await });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_sent_to_the_follower_is_confirmed_on_both_nodes() {
    let (a_dir, b_dir) = (
        tempfile::tempdir().expect("a"),
        tempfile::tempdir().expect("b"),
    );
    let (alice, genesis) = setup_validator(a_dir.path(), 7);
    let a = Node::open(a_dir.path()).expect("validator");
    let url = serve(Arc::clone(&a)).await;

    // The follower fetches genesis from the leader and checks the pinned hash.
    let b = Node::open_follower(b_dir.path(), &url, genesis.hash())
        .await
        .expect("follower");

    // Sent to the follower, which verifies it and forwards it to the leader.
    let first = pay(&alice, 7, 0);
    let id = b.submit(first.clone()).await.expect("submit via follower");
    assert_eq!(id, first.id());
    // A follow-up, before the first is in a block: the leader verifies it
    // against the key queued with the first.
    b.submit(pay(&alice, 7, 1))
        .await
        .expect("second via follower");

    assert_eq!(a.produce_block().expect("produce"), Some(1));
    assert_eq!(b.sync_once().await.expect("sync"), 1);

    assert_eq!(b.state.read().height(), 1);
    assert_eq!(b.state.read().state_root(), a.state.read().state_root());
    assert_eq!(b.state.read().head(), a.state.read().head());
    assert_eq!(
        b.store.read().locate(&first.id()).map(|l| l.height),
        Some(1)
    );

    // An invalid transaction is refused by the follower and never reaches the leader.
    let replay = pay(&alice, 7, 0);
    assert!(b.submit(replay).await.is_err());
    assert!(a.mempool.lock().is_empty());

    // Nothing new: nothing to apply.
    assert_eq!(b.sync_once().await.expect("sync"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_follower_restarts_from_its_own_verified_log() {
    let (a_dir, b_dir) = (
        tempfile::tempdir().expect("a"),
        tempfile::tempdir().expect("b"),
    );
    let (alice, genesis) = setup_validator(a_dir.path(), 8);
    let a = Node::open(a_dir.path()).expect("validator");
    let url = serve(Arc::clone(&a)).await;

    let b = Node::open_follower(b_dir.path(), &url, genesis.hash())
        .await
        .expect("follower");
    for nonce in 0..3 {
        a.submit(pay(&alice, 8, nonce)).await.expect("submit");
        a.produce_block().expect("produce");
    }
    assert_eq!(b.sync_once().await.expect("sync"), 3);
    drop(b);

    let b = Node::open_follower(b_dir.path(), &url, genesis.hash())
        .await
        .expect("reopen");
    assert_eq!(b.state.read().height(), 3);
    assert_eq!(b.state.read().state_root(), a.state.read().state_root());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_follower_refuses_a_leader_on_another_genesis() {
    let (a_dir, b_dir) = (
        tempfile::tempdir().expect("a"),
        tempfile::tempdir().expect("b"),
    );
    setup_validator(a_dir.path(), 9);
    let a = Node::open(a_dir.path()).expect("validator");
    let url = serve(a).await;

    let wrong = Hash([1; 32]);
    let refused = Node::open_follower(b_dir.path(), &url, wrong).await;
    assert!(matches!(refused, Err(NodeError::Genesis(_))));
    assert!(
        !b_dir.path().join(files::GENESIS).exists(),
        "an unpinned genesis is never saved"
    );
}
