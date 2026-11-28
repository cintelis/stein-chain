//! The RPC's per-client rate limits, over real HTTP: who is counted, what a
//! client over budget is told, and what a follower makes of being told it.

#![allow(clippy::arithmetic_side_effects)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use stein_crypto::{Address, Scheme, SecretKey, create_seed_file};
use stein_node::limit::{Limiter, Limits};
use stein_node::{Node, files, rpc};
use stein_types::{
    Action, Allocation, ChainParams, Genesis, GenesisValidator, ONE_STEIN, Transaction, TxBody,
};

/// The header the tests' "proxy" names the client in.
const CLIENT: &str = "x-client-ip";

fn setup(dir: &std::path::Path) -> (SecretKey, Genesis) {
    let validator =
        create_seed_file(&dir.join(files::VALIDATOR_SEED), Scheme::MlDsa44).expect("validator");
    let alice = SecretKey::from_seed(Scheme::MlDsa44, &[42; 32]).expect("alice");
    let genesis = Genesis {
        chain_id: 12,
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

fn pay(alice: &SecretKey, nonce: u64) -> Transaction {
    let body = TxBody {
        version: TxBody::VERSION,
        chain_id: 12,
        sender: alice.address(),
        nonce,
        fee: ONE_STEIN / 100,
        expires_at: 50,
        action: Action::Transfer {
            to: Address([9; 32]),
            amount: ONE_STEIN,
        },
    };
    Transaction::sign(body, alice, nonce == 0).expect("sign")
}

/// Serves `node` as the binary does: limited, and knowing each peer's address.
async fn serve(node: Arc<Node>, limits: Limits) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = rpc::router_with_limits(node, Limiter::new(limits));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    format!("http://{addr}")
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .use_preconfigured_tls(stein_tls::client_config().expect("tls"))
        .build()
        .expect("client")
}

fn behind_proxy(limits: Limits) -> Limits {
    Limits {
        client_ip_header: Some(CLIENT.parse().expect("header name")),
        ..limits
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_over_budget_is_told_to_wait_and_others_are_not_affected() {
    let dir = tempfile::tempdir().expect("dir");
    setup(dir.path());
    let node = Node::open(dir.path()).expect("validator");
    let url = serve(
        node,
        behind_proxy(Limits {
            reads_per_minute: 3,
            ..Limits::default()
        }),
    )
    .await;
    let http = client();
    let status = |ip: &'static str| {
        let request = http
            .get(format!("{url}/status"))
            .header(CLIENT, ip)
            .header("origin", "https://steinscan.io");
        async move { request.send().await.expect("status") }
    };

    for _ in 0..3 {
        assert_eq!(status("203.0.113.5").await.status(), 200);
    }
    let refused = status("203.0.113.5").await;
    assert_eq!(refused.status(), 429);
    // 3 a minute is one every 20 s.
    assert_eq!(refused.headers()["retry-after"], "20");
    // A page on another origin can read the refusal and how long to wait.
    assert_eq!(refused.headers()["access-control-allow-origin"], "*");
    assert_eq!(
        refused.headers()["access-control-expose-headers"],
        "retry-after"
    );
    let body: serde_json::Value = refused.json().await.expect("json");
    assert!(body["error"].as_str().expect("error").contains("too many"));

    // Another client has a budget of its own; so has another kind of request.
    assert_eq!(status("203.0.113.6").await.status(), 200);
    let sync = http
        .get(format!("{url}/raw/blocks?from=1"))
        .header(CLIENT, "203.0.113.5")
        .send()
        .await
        .expect("sync");
    assert_eq!(sync.status(), 200);

    // This machine itself, reaching the node directly, is not limited.
    for _ in 0..10 {
        let local = http
            .get(format!("{url}/status"))
            .send()
            .await
            .expect("status");
        assert_eq!(local.status(), 200);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn submissions_are_limited_before_any_signature_is_checked() {
    let dir = tempfile::tempdir().expect("dir");
    let (alice, _) = setup(dir.path());
    let node = Node::open(dir.path()).expect("validator");
    let url = serve(
        Arc::clone(&node),
        behind_proxy(Limits {
            submits_per_minute: 2,
            ..Limits::default()
        }),
    )
    .await;
    let http = client();
    let submit = |nonce: u64| {
        let raw = hex::encode(borsh::to_vec(&pay(&alice, nonce)).expect("borsh"));
        let request = http
            .post(format!("{url}/tx"))
            .header(CLIENT, "203.0.113.5")
            .json(&rpc::SubmitRequest { raw });
        async move { request.send().await.expect("submit").status() }
    };
    assert_eq!(submit(0).await, 200);
    assert_eq!(submit(1).await, 200);
    assert_eq!(submit(2).await, 429);
    // The refused transaction was never looked at.
    assert_eq!(node.mempool.lock().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn event_streams_are_capped_per_client_and_a_closed_one_frees_its_slot() {
    let dir = tempfile::tempdir().expect("dir");
    setup(dir.path());
    let node = Node::open(dir.path()).expect("validator");
    let url = serve(
        Arc::clone(&node),
        behind_proxy(Limits {
            streams: 1,
            ..Limits::default()
        }),
    )
    .await;
    let http = client();
    let open = || {
        let request = http
            .get(format!("{url}/events"))
            .header(CLIENT, "203.0.113.5");
        async move { request.send().await.expect("events") }
    };

    let first = open().await;
    assert_eq!(first.status(), 200);
    assert_eq!(open().await.status(), 429);
    // Other requests from the same client are still served.
    let status = http
        .get(format!("{url}/status"))
        .header(CLIENT, "203.0.113.5")
        .send()
        .await
        .expect("status");
    assert_eq!(status.status(), 200);

    drop(first);
    let mut reopened = false;
    for _ in 0..100 {
        if open().await.status() == 200 {
            reopened = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(reopened, "the closed stream's slot was not freed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_follower_told_to_wait_retries_instead_of_giving_up() {
    // A leader that answers every request as a node over budget does.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let busy = axum::Router::new().fallback(|| async {
        (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "1")],
            "{\"error\":\"too many requests\"}",
        )
    });
    tokio::spawn(async move { axum::serve(listener, busy).await });

    let (a_dir, b_dir) = (
        tempfile::tempdir().expect("a"),
        tempfile::tempdir().expect("b"),
    );
    let (_, genesis) = setup(a_dir.path());
    std::fs::copy(
        a_dir.path().join(files::GENESIS),
        b_dir.path().join(files::GENESIS),
    )
    .expect("genesis");
    let b = Node::open_follower(b_dir.path(), &url, genesis.hash())
        .await
        .expect("follower");
    // Trouble reaching the leader, which is retried; not data that fails
    // verification, which would stop the follower for good.
    assert!(matches!(
        b.sync_once().await,
        Err(stein_node::NodeError::Leader(_))
    ));
    assert_eq!(*b.sync_error.lock(), None);
}
