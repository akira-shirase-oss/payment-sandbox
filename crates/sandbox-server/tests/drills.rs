//! Drill (a) and the both-processes restart (D-026, ROADMAP M3): the server runs as a real
//! child process, built with the `fault-injection` feature, and is killed at the worst
//! moments — after the simulated network accepted an operation but before the answer was
//! stored, and before the network was called at all. A fresh process recovers by fenced
//! readback, never by blind replay: no second charge, no second capture.
//!
//! Run with `cargo test -p sandbox-server --features fault-injection --test drills`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{BOOTSTRAP, TestSim};
use sandbox_db::testing::TestDb;
use serde_json::{Value, json};

struct Server {
    child: Child,
    base: String,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Server {
    async fn start(db: &TestDb, sim_url: &str, port: u16, extra: &[(&str, &str)]) -> Self {
        let log =
            std::fs::File::create(std::env::temp_dir().join(format!("ps-drill-server-{port}.log")))
                .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_sandbox-server"));
        command
            .env("PS_DATABASE_URL", db.url())
            .env("PS_LISTEN", format!("127.0.0.1:{port}"))
            .env("PS_SIM_URL", sim_url)
            .env("PS_SIM_TOKEN", common::SIM_TOKEN)
            .env("PS_BOOTSTRAP_TOKEN", BOOTSTRAP)
            .env("PS_RECONCILE_GRACE_MS", "1")
            .env("PS_RECONCILE_INTERVAL_MS", "200")
            .env("PS_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(log);
        for (k, v) in extra {
            command.env(k, v);
        }
        let child = command.spawn().unwrap();
        let server = Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
        };
        server.wait_ready().await;
        server
    }

    async fn wait_ready(&self) {
        let http = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(r) = http.get(format!("{}/readyz", self.base)).send().await
                && r.status().is_success()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the server never became ready");
    }

    /// Waits for the process to end on its own (the fault point aborts it).
    fn wait_for_crash(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    !status.success(),
                    "the server exited cleanly instead of crashing"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the fault point never fired");
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn call(
    base: &str,
    method: reqwest::Method,
    path: &str,
    key: &str,
    idem: Option<&str>,
    body: Option<Value>,
) -> Result<(u16, Value), reqwest::Error> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let mut request = http
        .request(method, format!("{base}{path}"))
        .bearer_auth(key);
    if let Some(idem) = idem {
        request = request.header("idempotency-key", idem);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    Ok((status, body))
}

async fn project(base: &str) -> String {
    let (status, body) = call(
        base,
        reqwest::Method::POST,
        "/v1/projects",
        BOOTSTRAP,
        None,
        Some(json!({"slug": "drill", "name": "Drill"})),
    )
    .await
    .unwrap();
    assert_eq!(status, 201, "{body}");
    body["api_key"]["secret"].as_str().unwrap().to_owned()
}

async fn payment_until(base: &str, key: &str, pid: &str, field: &str, want: &Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (_, body) = call(
            base,
            reqwest::Method::GET,
            &format!("/v1/payments/{pid}"),
            key,
            None,
            None,
        )
        .await
        .unwrap();
        if &body["payment"][field] == want {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "{field} never became {want}: {body}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn assert_clean(base: &str, key: &str) {
    let (_, report) = call(
        base,
        reqwest::Method::GET,
        "/v1/reconciliation?network=true",
        key,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(report["clean"], true, "{report}");
    assert_eq!(report["report"]["open_operations"], 0, "{report}");
}

/// The guide's drill: kill the server after the simulated network accepts the operation but
/// before the response is stored; recover without another logical charge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drill_a_server_killed_after_the_network_accepted_an_authorization_recovers_without_a_second_charge()
 {
    let sim = TestSim::start().await;
    let db = TestDb::new().await;
    let port = free_port();
    let mut server = Server::start(
        &db,
        &sim.url,
        port,
        &[
            ("PS_FAULT", "after_network"),
            ("PS_FAULT_KIND", "authorize"),
        ],
    )
    .await;
    let key = project(&server.base).await;
    let body = json!({"amount": 1_200, "currency": "USD", "token": "tok_sim_approve"});
    let crashed = call(
        &server.base,
        reqwest::Method::POST,
        "/v1/payments",
        &key,
        Some("order-9"),
        Some(body.clone()),
    )
    .await;
    assert!(
        crashed.is_err(),
        "the request must die with the server: {crashed:?}"
    );
    server.wait_for_crash();
    assert_eq!(
        sim.store.count().await.unwrap(),
        1,
        "the network executed the authorization"
    );

    let server = Server::start(&db, &sim.url, port, &[]).await;
    // The restarted server's reconciler finds the open operation and applies the network's
    // record. The client, meanwhile, retries with the same key.
    let (status, again) = call(
        &server.base,
        reqwest::Method::POST,
        "/v1/payments",
        &key,
        Some("order-9"),
        Some(body),
    )
    .await
    .unwrap();
    assert!(status == 200 || status == 201, "{status} {again}");
    let pid = again["payment"]["id"].as_str().unwrap().to_owned();
    payment_until(&server.base, &key, &pid, "status", &json!("authorized")).await;
    assert_eq!(sim.store.count().await.unwrap(), 1, "no second charge");
    let (_, journal) = call(
        &server.base,
        reqwest::Method::GET,
        &format!("/v1/payments/{pid}/journal"),
        &key,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        journal["entries"].as_array().unwrap().len(),
        1,
        "one authorization posted, once"
    );
    assert_clean(&server.base, &key).await;
    server.kill();
    db.cleanup().await;
    sim.remove().await;
}

/// The same drill for a capture, recovered by the reconciler alone (no client retry).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drill_a_server_killed_after_the_network_accepted_a_capture_recovers_it_once() {
    let sim = TestSim::start().await;
    let db = TestDb::new().await;
    let port = free_port();
    let server = Server::start(&db, &sim.url, port, &[]).await;
    let key = project(&server.base).await;
    let (_, auth) = call(
        &server.base,
        reqwest::Method::POST,
        "/v1/payments",
        &key,
        Some("o"),
        Some(json!({"amount": 500, "currency": "EUR", "token": "tok_sim_approve"})),
    )
    .await
    .unwrap();
    let pid = auth["payment"]["id"].as_str().unwrap().to_owned();
    server.kill();

    let mut server = Server::start(
        &db,
        &sim.url,
        port,
        &[("PS_FAULT", "after_network"), ("PS_FAULT_KIND", "capture")],
    )
    .await;
    let crashed = call(
        &server.base,
        reqwest::Method::POST,
        &format!("/v1/payments/{pid}/captures"),
        &key,
        Some("cap"),
        Some(json!({"amount": 350})),
    )
    .await;
    assert!(crashed.is_err());
    server.wait_for_crash();
    assert_eq!(sim.store.count().await.unwrap(), 2);

    let server = Server::start(&db, &sim.url, port, &[]).await;
    let payment = payment_until(&server.base, &key, &pid, "captured", &json!(350)).await;
    assert_eq!(payment["payment"]["reserved"]["capture"], 0);
    // A retry under the same key replays; it never captures again.
    let (status, replay) = call(
        &server.base,
        reqwest::Method::POST,
        &format!("/v1/payments/{pid}/captures"),
        &key,
        Some("cap"),
        Some(json!({"amount": 350})),
    )
    .await
    .unwrap();
    assert_eq!((status, replay["replayed"].as_bool()), (200, Some(true)));
    assert_eq!(sim.store.count().await.unwrap(), 2);
    assert_clean(&server.base, &key).await;
    server.kill();
    db.cleanup().await;
    sim.remove().await;
}

/// Killed after the identity committed but before the network was called: the restarted
/// reconciler fences the identity, the reservation is released, nothing was charged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_killed_before_the_network_call_fences_the_identity_and_releases_the_reservation()
{
    let sim = TestSim::start().await;
    let db = TestDb::new().await;
    let port = free_port();
    let server = Server::start(&db, &sim.url, port, &[]).await;
    let key = project(&server.base).await;
    let (_, auth) = call(
        &server.base,
        reqwest::Method::POST,
        "/v1/payments",
        &key,
        Some("o"),
        Some(json!({"amount": 500, "currency": "USD", "token": "tok_sim_approve"})),
    )
    .await
    .unwrap();
    let pid = auth["payment"]["id"].as_str().unwrap().to_owned();
    server.kill();

    let mut server = Server::start(
        &db,
        &sim.url,
        port,
        &[("PS_FAULT", "before_network"), ("PS_FAULT_KIND", "capture")],
    )
    .await;
    let crashed = call(
        &server.base,
        reqwest::Method::POST,
        &format!("/v1/payments/{pid}/captures"),
        &key,
        Some("cap"),
        Some(json!({"amount": 200})),
    )
    .await;
    assert!(crashed.is_err());
    server.wait_for_crash();
    assert_eq!(
        sim.store.count().await.unwrap(),
        1,
        "the capture never reached the network"
    );

    let server = Server::start(&db, &sim.url, port, &[]).await;
    let payment = payment_until(&server.base, &key, &pid, "capturable", &json!(500)).await;
    let capture = payment["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "capture")
        .unwrap()
        .clone();
    assert_eq!(capture["state"], "not_executed");
    let (status, _) = call(
        &server.base,
        reqwest::Method::POST,
        &format!("/v1/payments/{pid}/captures"),
        &key,
        Some("cap-2"),
        Some(json!({"amount": 500})),
    )
    .await
    .unwrap();
    assert_eq!(status, 201, "the released capacity is usable");
    assert_clean(&server.base, &key).await;
    server.kill();
    db.cleanup().await;
    sim.remove().await;
}

/// Both processes restart mid-operation: the network executed an authorization and was
/// holding its reply when the server was killed; then the network restarts too. The
/// network's record survives, and the new server applies it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restarting_both_processes_mid_operation_applies_the_networks_record_once() {
    let path = std::env::temp_dir().join(format!(
        "ps-drill-sim-{}.redb",
        uuid::Uuid::now_v7().simple()
    ));
    let sim = TestSim::start_on("127.0.0.1:0", path, Duration::from_secs(20)).await;
    let addr = sim.addr();
    let db = TestDb::new().await;
    let port = free_port();
    let server = Server::start(
        &db,
        &sim.url,
        port,
        &[
            ("PS_SIM_TIMEOUT_MS", "8000"),
            ("PS_REQUEST_TIMEOUT_MS", "20000"),
        ],
    )
    .await;
    let key = project(&server.base).await;
    let base = server.base.clone();
    let key2 = key.clone();
    let in_flight = tokio::spawn(async move {
        call(
            &base,
            reqwest::Method::POST,
            "/v1/payments",
            &key2,
            Some("both"),
            Some(json!({"amount": 777, "currency": "JPY", "token": "tok_sim_lost_reply"})),
        )
        .await
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while sim.store.count().await.unwrap() == 0 {
        assert!(Instant::now() < deadline, "the network never executed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.kill();
    assert!(in_flight.await.unwrap().is_err());
    let path = sim.stop().await;

    let sim = TestSim::start_on(&addr, path, Duration::from_secs(3)).await;
    let server = Server::start(&db, &sim.url, port, &[]).await;
    let (_, list) = call(
        &server.base,
        reqwest::Method::GET,
        "/v1/payments",
        &key,
        None,
        None,
    )
    .await
    .unwrap();
    let pid = list["payments"][0]["id"].as_str().unwrap().to_owned();
    payment_until(&server.base, &key, &pid, "status", &json!("authorized")).await;
    assert_eq!(sim.store.count().await.unwrap(), 1);
    assert_clean(&server.base, &key).await;
    server.kill();
    db.cleanup().await;
    sim.remove().await;
}

/// Review finding 19: the reconciler and the deliverer were plain `tokio::spawn`, so a
/// panic ended them silently: no restart, `/readyz` green, and uncertain operations never
/// resolved. Here the real binary's reconciler panics on its first sweep; it is restarted
/// and resolves an uncertain operation, and readiness reports the restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_reconciler_is_restarted_and_still_resolves_uncertain_operations() {
    let sim = TestSim::start().await;
    let db = TestDb::new().await;
    let port = free_port();
    let server = Server::start(&db, &sim.url, port, &[("PS_FAULT", "reconciler_panic")]).await;
    let key = project(&server.base).await;
    sim.fault(json!({"kind": "authorize", "target": null, "action": "unavailable", "count": 1}));
    let (status, body) = call(
        &server.base,
        reqwest::Method::POST,
        "/v1/payments",
        &key,
        Some("order-1"),
        Some(json!({"amount": 500, "currency": "USD", "token": "tok_sim_approve"})),
    )
    .await
    .unwrap();
    assert_eq!(status, 202, "{body}");
    let pid = body["payment"]["id"].as_str().unwrap().to_owned();
    // Only a running reconciler can fence and resolve it.
    payment_until(&server.base, &key, &pid, "status", &json!("failed")).await;
    let (status, ready) = call(
        &server.base,
        reqwest::Method::GET,
        "/readyz",
        &key,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, 200, "{ready}");
    let reconciler = ready["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "reconciler")
        .unwrap()
        .clone();
    assert_eq!(reconciler["restarts"], 1, "{ready}");
    assert_eq!(reconciler["last_failure"], "panicked", "{ready}");
    server.kill();
    db.cleanup().await;
    sim.remove().await;
}

/// A checkout origin's call: the payer's page, with its session secret.
async fn checkout_call(
    base: &str,
    path: &str,
    secret: &str,
    idem: Option<&str>,
    body: Option<Value>,
) -> Result<(u16, Value), reqwest::Error> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let mut request = match &body {
        Some(_) => http.post(format!("{base}{path}")).header("origin", base),
        None => http.get(format!("{base}{path}")),
    }
    .bearer_auth(secret);
    if let Some(idem) = idem {
        request = request.header("idempotency-key", idem);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    Ok((
        status,
        response.json::<Value>().await.unwrap_or(Value::Null),
    ))
}

async fn checkout_until(base: &str, id: &str, secret: &str, want: (&str, Option<&str>)) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, view) = checkout_call(base, &format!("/api/sessions/{id}"), secret, None, None)
            .await
            .unwrap();
        let seen = (
            view["session"]["state"].as_str().unwrap_or(""),
            view["session"]["next_action"].as_str(),
        );
        if seen == want {
            return view;
        }
        assert_ne!(seen.0, "paid", "paid before the drill expected it: {view}");
        assert!(
            Instant::now() < deadline,
            "the session never reached {want:?}: {view}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The M3 kill drill through the checkout path (ROADMAP M6): the payer's "Pay" reaches the
/// network, which approves, and the server dies before it stores the answer. The restarted
/// server's reconciler applies the network's record; the page continues; the capture runs
/// once. One payment, one authorization and one capture at the network, nothing twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(clippy::too_many_lines, reason = "one drill, start to finish")]
async fn drill_a_through_the_checkout_a_payer_whose_server_died_mid_payment_is_charged_once() {
    let sim = TestSim::start().await;
    let db = TestDb::new().await;
    let (site_dir, _) = common::test_site();
    let (port, checkout_port) = (free_port(), free_port());
    let site = [
        ("PS_CHECKOUT_DIR", site_dir.to_str().unwrap().to_owned()),
        ("PS_CHECKOUT_LISTEN", format!("127.0.0.1:{checkout_port}")),
    ];
    let with = |extra: &[(&'static str, &'static str)]| {
        let mut vars: Vec<(&str, &str)> = site.iter().map(|(k, v)| (*k, v.as_str())).collect();
        vars.extend_from_slice(extra);
        vars
    };
    let checkout = format!("http://127.0.0.1:{checkout_port}");
    let mut server = Server::start(
        &db,
        &sim.url,
        port,
        &with(&[
            ("PS_FAULT", "after_network"),
            ("PS_FAULT_KIND", "authorize"),
        ]),
    )
    .await;
    let key = project(&server.base).await;
    let (status, created) = call(
        &server.base,
        reqwest::Method::POST,
        "/v1/checkout_sessions",
        &key,
        None,
        Some(json!({"amount": 2_500, "currency": "USD", "description": "drill order"})),
    )
    .await
    .unwrap();
    assert_eq!(status, 201, "{created}");
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    let secret = created["client_secret"].as_str().unwrap().to_owned();
    assert!(created["url"].as_str().unwrap().starts_with(&checkout));

    let pay_key = "8b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001";
    let crashed = checkout_call(
        &checkout,
        &format!("/api/sessions/{id}/attempts"),
        &secret,
        Some(pay_key),
        Some(json!({"token": "tok_sim_approve"})),
    )
    .await;
    assert!(
        crashed.is_err(),
        "the payer's request dies with the server: {crashed:?}"
    );
    server.wait_for_crash();
    assert_eq!(
        sim.store.count().await.unwrap(),
        1,
        "the network approved the hold"
    );

    let server = Server::start(&db, &sim.url, port, &with(&[])).await;
    // The reconciler applies the network's record; the page is told to continue.
    checkout_until(&checkout, &id, &secret, ("authorized", Some("continue"))).await;
    // The payer's retry with the same key (or the page's continue) captures once.
    let (status, paid) = checkout_call(
        &checkout,
        &format!("/api/sessions/{id}/attempts"),
        &secret,
        Some(pay_key),
        Some(json!({"token": "tok_sim_approve"})),
    )
    .await
    .unwrap();
    assert_eq!(
        (status, paid["session"]["state"].as_str()),
        (200, Some("paid")),
        "{paid}"
    );
    let (_, again) = checkout_call(
        &checkout,
        &format!("/api/sessions/{id}/continue"),
        &secret,
        None,
        Some(json!({})),
    )
    .await
    .unwrap();
    assert_eq!(again["session"]["state"], "paid");
    assert_eq!(
        sim.store.count().await.unwrap(),
        2,
        "one authorization, one capture"
    );
    let (_, merchant) = call(
        &server.base,
        reqwest::Method::GET,
        &format!("/v1/checkout_sessions/{id}"),
        &key,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        merchant["attempts"].as_array().unwrap().len(),
        1,
        "{merchant}"
    );
    let payment_id = merchant["attempts"][0]["payment_id"].as_str().unwrap();
    let (_, journal) = call(
        &server.base,
        reqwest::Method::GET,
        &format!("/v1/payments/{payment_id}/journal"),
        &key,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        journal["entries"].as_array().unwrap().len(),
        2,
        "authorize and capture, once each"
    );
    assert_clean(&server.base, &key).await;
    server.kill();
    let _ = std::fs::remove_dir_all(site_dir);
    db.cleanup().await;
    sim.remove().await;
}

/// The same drill at the capture: the network captured, the server died before storing
/// it; the reconciler records the capture and the page shows paid with no second capture.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drill_a_through_the_checkout_a_capture_lost_to_a_crash_is_recorded_once() {
    let sim = TestSim::start().await;
    let db = TestDb::new().await;
    let (site_dir, _) = common::test_site();
    let (port, checkout_port) = (free_port(), free_port());
    let dir = site_dir.to_str().unwrap().to_owned();
    let listen = format!("127.0.0.1:{checkout_port}");
    let checkout = format!("http://{listen}");
    let mut server = Server::start(
        &db,
        &sim.url,
        port,
        &[
            ("PS_CHECKOUT_DIR", &dir),
            ("PS_CHECKOUT_LISTEN", &listen),
            ("PS_FAULT", "after_network"),
            ("PS_FAULT_KIND", "capture"),
        ],
    )
    .await;
    let key = project(&server.base).await;
    let (_, created) = call(
        &server.base,
        reqwest::Method::POST,
        "/v1/checkout_sessions",
        &key,
        None,
        Some(json!({"amount": 800, "currency": "EUR"})),
    )
    .await
    .unwrap();
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    let secret = created["client_secret"].as_str().unwrap().to_owned();
    let crashed = checkout_call(
        &checkout,
        &format!("/api/sessions/{id}/attempts"),
        &secret,
        Some("9b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001"),
        Some(json!({"token": "tok_sim_approve"})),
    )
    .await;
    assert!(crashed.is_err(), "{crashed:?}");
    server.wait_for_crash();
    assert_eq!(
        sim.store.count().await.unwrap(),
        2,
        "authorized and captured at the network"
    );

    let server = Server::start(
        &db,
        &sim.url,
        port,
        &[("PS_CHECKOUT_DIR", &dir), ("PS_CHECKOUT_LISTEN", &listen)],
    )
    .await;
    checkout_until(&checkout, &id, &secret, ("paid", None)).await;
    let (_, again) = checkout_call(
        &checkout,
        &format!("/api/sessions/{id}/continue"),
        &secret,
        None,
        Some(json!({})),
    )
    .await
    .unwrap();
    assert_eq!(again["session"]["state"], "paid");
    assert_eq!(sim.store.count().await.unwrap(), 2, "no second capture");
    assert_clean(&server.base, &key).await;
    server.kill();
    let _ = std::fs::remove_dir_all(site_dir);
    db.cleanup().await;
    sim.remove().await;
}
