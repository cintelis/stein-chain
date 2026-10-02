//! What steinscan relies on: live events (server-sent, over real HTTP) and the
//! list and lookup endpoints.

#![allow(clippy::arithmetic_side_effects)]

use std::sync::Arc;
use std::time::Duration;

use stein_crypto::{Address, Scheme, SecretKey, create_seed_file};
use stein_node::views::{BlockView, Event, TxView};
use stein_node::{Node, files, rpc};
use stein_types::{
    Action, Allocation, ChainParams, Genesis, GenesisValidator, ONE_STEIN, Transaction, TxBody,
};

fn setup(dir: &std::path::Path) -> (SecretKey, Genesis) {
    let validator =
        create_seed_file(&dir.join(files::VALIDATOR_SEED), Scheme::MlDsa44).expect("validator");
    let alice = SecretKey::from_seed(Scheme::MlDsa44, &[42; 32]).expect("alice");
    let genesis = Genesis {
        chain_id: 11,
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

fn pay(alice: &SecretKey, nonce: u64, to: Address) -> Transaction {
    let body = TxBody {
        version: TxBody::VERSION,
        chain_id: 11,
        sender: alice.address(),
        nonce,
        fee: ONE_STEIN / 100,
        expires_at: 50,
        action: Action::Transfer {
            to,
            amount: ONE_STEIN,
        },
    };
    Transaction::sign(body, alice, nonce == 0).expect("sign")
}

async fn serve(node: Arc<Node>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, rpc::router(node)).await });
    format!("http://{addr}")
}

async fn get<T: serde::de::DeserializeOwned>(url: &str) -> T {
    reqwest::Client::builder()
        .use_preconfigured_tls(stein_tls::client_config().expect("tls"))
        .build()
        .expect("client")
        .get(url)
        .send()
        .await
        .expect("get")
        .json()
        .await
        .expect("json")
}

#[tokio::test(flavor = "multi_thread")]
async fn events_announce_pending_transactions_then_their_block() {
    let (a_dir, b_dir) = (
        tempfile::tempdir().expect("a"),
        tempfile::tempdir().expect("b"),
    );
    let (alice, genesis) = setup(a_dir.path());
    let a = Node::open(a_dir.path()).expect("validator");
    let url = serve(Arc::clone(&a)).await;
    let b = Node::open_follower(b_dir.path(), &url, genesis.hash())
        .await
        .expect("follower");

    let mut on_a = a.events.subscribe();
    let mut on_b = b.events.subscribe();
    let tx = pay(&alice, 0, Address([9; 32]));
    a.submit(tx.clone()).await.expect("submit");
    match on_a.recv().await.expect("event") {
        Event::Pending(view) => {
            assert_eq!(view.id, tx.id());
            assert_eq!(view.scheme, Some(Scheme::MlDsa44));
            assert_eq!(view.registers_key, Some(true));
            assert_eq!(view.height, None);
        }
        other => panic!("expected pending, got {other:?}"),
    }

    a.produce_block().expect("produce");
    match on_a.recv().await.expect("event") {
        Event::Block(block) => {
            assert_eq!(block.height, 1);
            assert_eq!(block.txs[0].id, tx.id());
            assert_eq!(block.txs[0].height, Some(1));
            assert_eq!(block.size, Some(tx.encoded_len()));
        }
        other => panic!("expected block, got {other:?}"),
    }

    // A follower announces the block once it has verified it.
    b.sync_once().await.expect("sync");
    assert!(matches!(on_b.recv().await.expect("event"), Event::Block(v) if v.height == 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn events_stream_over_http_as_server_sent_events() {
    let dir = tempfile::tempdir().expect("dir");
    let (alice, _) = setup(dir.path());
    let a = Node::open(dir.path()).expect("validator");
    let url = serve(Arc::clone(&a)).await;

    let client = reqwest::Client::builder()
        .use_preconfigured_tls(stein_tls::client_config().expect("tls"))
        .build()
        .expect("client");
    let mut response = client
        .get(format!("{url}/events"))
        .send()
        .await
        .expect("events");
    assert_eq!(
        response.headers()["content-type"].to_str().expect("type"),
        "text/event-stream"
    );
    assert_eq!(response.headers()["access-control-allow-origin"], "*");

    // Wait until the subscription exists, then make something happen.
    for _ in 0..50 {
        if a.events.receiver_count() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    a.submit(pay(&alice, 0, Address([9; 32])))
        .await
        .expect("submit");
    a.produce_block().expect("produce");

    let mut text = String::new();
    while !text.contains("event: block") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), response.chunk())
            .await
            .expect("an event within 5 s")
            .expect("chunk")
            .expect("stream open");
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(text.contains("event: pending"));
    let data = text
        .lines()
        .skip_while(|l| *l != "event: block")
        .find_map(|l| l.strip_prefix("data: "))
        .expect("block data");
    match serde_json::from_str::<Event>(data).expect("event json") {
        Event::Block(b) => assert_eq!((b.height, b.tx_count), (1, 1)),
        other => panic!("expected block, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_and_lookups_for_the_explorer() {
    let dir = tempfile::tempdir().expect("dir");
    let (alice, _) = setup(dir.path());
    let a = Node::open(dir.path()).expect("validator");
    let url = serve(Arc::clone(&a)).await;
    let bob = Address([7; 32]);

    let first = pay(&alice, 0, bob);
    a.submit(first.clone()).await.expect("submit");
    let pending: serde_json::Value = get(&format!("{url}/tx/{}", first.id())).await;
    assert_eq!(pending["status"], "pending");
    let pool: Vec<TxView> = get(&format!("{url}/mempool")).await;
    assert_eq!(pool.len(), 1);

    a.produce_block().expect("produce");
    for nonce in 1..3 {
        a.submit(pay(&alice, nonce, bob)).await.expect("submit");
        a.produce_block().expect("produce");
    }

    let included: serde_json::Value = get(&format!("{url}/tx/{}", first.id())).await;
    assert_eq!(included["status"], "included");
    assert_eq!(included["tx"]["amount_stein"], "1");

    let recent: Vec<BlockView> = get(&format!("{url}/blocks?limit=2")).await;
    assert_eq!(recent.iter().map(|b| b.height).collect::<Vec<_>>(), [3, 2]);
    let older: Vec<BlockView> = get(&format!("{url}/blocks?before=1")).await;
    assert_eq!(older.iter().map(|b| b.height).collect::<Vec<_>>(), [1]);

    let history: Vec<TxView> = get(&format!("{url}/account/{bob}/txs")).await;
    assert_eq!(
        history.iter().map(|t| t.nonce).collect::<Vec<_>>(),
        [2, 1, 0]
    );
    let sent: Vec<TxView> = get(&format!("{url}/account/{}/txs?limit=1", alice.address())).await;
    assert_eq!(sent.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn totals_validators_and_top_accounts() {
    let dir = tempfile::tempdir().expect("dir");
    let (alice, _) = setup(dir.path());
    let a = Node::open(dir.path()).expect("validator");
    let url = serve(Arc::clone(&a)).await;
    let bob = Address([7; 32]);
    for nonce in 0..3 {
        a.submit(pay(&alice, nonce, bob)).await.expect("submit");
    }
    a.produce_block().expect("produce");

    let status: rpc::Status = get(&format!("{url}/status")).await;
    assert_eq!(status.tx_total, 3);
    assert_eq!(status.total_supply_stein, "100");
    assert_eq!(status.accounts, 3, "alice, bob and the validator (fees)");

    let blocks: Vec<BlockView> = get(&format!("{url}/blocks?limit=1")).await;
    assert_eq!(blocks[0].fees_stein, "0.03");

    let validators: serde_json::Value = get(&format!("{url}/validators")).await;
    assert_eq!(validators["validators"].as_array().map(Vec::len), Some(1));
    assert_eq!(validators["validators"][0]["scheme"], "ml-dsa-44");

    let top: Vec<rpc::AccountView> = get(&format!("{url}/accounts/top?limit=2")).await;
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].address, alice.address());
    assert_eq!(top[1].balance_stein, "3");
}
