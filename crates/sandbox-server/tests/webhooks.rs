//! M4: signed webhooks, end to end — the outbox, the leased deliverer, the egress rule, the
//! signature with its replay window and rotation, and `fixtures/dev-store` as the consumer
//! that deduplicates and fulfills under its own business key.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApp, TestSim};
use sandbox_dev_store::{DevStore, StoreConfig};
use sandbox_domain::webhook::{self, EventPayload};
use sandbox_server::deliverer::{Deliverer, DeliveryReport, ShiftedClock};
use serde_json::json;

const SECRET_KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

struct Running {
    url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Serves the in-process app on a real port, so the store can read payments over HTTP.
async fn serve_api(app: &TestApp) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let router = app.router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });
    Running {
        url,
        stop: Some(tx),
    }
}

async fn start_store(addr: &str, store: DevStore) -> Running {
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = sandbox_dev_store::serve(listener, store, async move {
            let _ = rx.await;
        })
        .await;
    });
    Running {
        url,
        stop: Some(tx),
    }
}

fn new_store() -> DevStore {
    let path = std::env::temp_dir().join(format!(
        "ps-dev-store-{}.redb",
        uuid::Uuid::now_v7().simple()
    ));
    DevStore::open(&path, StoreConfig::default()).unwrap()
}

async fn setup() -> (TestSim, TestApp, String) {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[("PS_SECRET_KEY", SECRET_KEY)]).await;
    let (_, key) = app.project("store").await;
    (sim, app, key)
}

async fn endpoint(app: &TestApp, key: &str, url: &str) -> (String, String) {
    let (status, reply) = app
        .post("/v1/webhook_endpoints", key, None, json!({"url": url}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    (
        reply["endpoint"]["id"].as_str().unwrap().to_owned(),
        reply["secret"].as_str().unwrap().to_owned(),
    )
}

/// Runs the deliverer until nothing is due.
async fn deliver_all(deliverer: &Deliverer) -> DeliveryReport {
    let mut total = DeliveryReport::default();
    for _ in 0..50 {
        let report = deliverer.run_once().await.unwrap();
        if report.claimed == 0 {
            break;
        }
        total.claimed += report.claimed;
        total.delivered += report.delivered;
        total.retried += report.retried;
        total.dead += report.dead;
        total.stale += report.stale;
    }
    total
}

async fn purchase(app: &TestApp, key: &str, n: usize, capture: i64) -> String {
    let (_, auth) = app
        .authorize(key, &format!("o{n}"), 1_000, "USD", "tok_sim_approve")
        .await;
    let pid = auth["payment"]["id"].as_str().unwrap().to_owned();
    if capture > 0 {
        let (status, _) = app
            .post(
                &format!("/v1/payments/{pid}/captures"),
                key,
                Some(&format!("c{n}")),
                json!({"amount": capture}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    pid
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_purchase_is_delivered_signed_and_fulfilled_exactly_once() {
    let (sim, app, key) = setup().await;
    let (_, reader) = app.key(&key, &["read"]).await;
    let api = serve_api(&app).await;
    let store = new_store();
    let running = start_store("127.0.0.1:0", store.clone()).await;
    let (_, secret) = endpoint(&app, &key, &format!("{}/webhooks", running.url)).await;
    store
        .configure(StoreConfig {
            sandbox_url: api.url.clone(),
            api_key: reader,
            secrets: vec![secret],
        })
        .await;

    let pid = purchase(&app, &key, 1, 600).await;
    app.post(
        &format!("/v1/payments/{pid}/refunds"),
        &key,
        Some("r1"),
        json!({"amount": 100}),
    )
    .await;
    let deliverer = Deliverer::new(&app.state, Arc::new(ShiftedClock::default())).unwrap();
    let report = deliver_all(&deliverer).await;
    assert_eq!(
        (report.delivered, report.retried, report.dead),
        (3, 0, 0),
        "{report:?}"
    );
    let (_, events) = app.get(&format!("/v1/events?payment_id={pid}"), &key).await;
    let events = events["events"].as_array().unwrap();
    let kinds: BTreeSet<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        BTreeSet::from([
            "authorize.succeeded",
            "capture.succeeded",
            "refund.succeeded"
        ])
    );
    let sequences: BTreeSet<i64> = events
        .iter()
        .map(|e| e["sequence"].as_i64().unwrap())
        .collect();
    assert_eq!(sequences, BTreeSet::from([1, 2, 3]));
    assert!(events.iter().all(|e| e["deliveries"][0][1] == "delivered"));
    let fulfillments = store.fulfillments().await.unwrap();
    assert_eq!(fulfillments.len(), 1);
    assert_eq!(fulfillments[0].payment_id.to_string(), pid);
    assert_eq!(store.inbox_len().await.unwrap(), 3);
    assert_eq!(
        deliver_all(&deliverer).await.claimed,
        0,
        "nothing is delivered twice by the outbox"
    );
    app.cleanup().await;
    sim.remove().await;
}

/// The receiver is down: attempts fail and back off; once it is up, everything arrives and
/// the order is fulfilled once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receiver_that_is_down_gets_retries_and_then_every_event() {
    let (sim, app, key) = setup().await;
    let (_, reader) = app.key(&key, &["read"]).await;
    let api = serve_api(&app).await;
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let (_, secret) = endpoint(&app, &key, &format!("http://{addr}/webhooks")).await;
    let pid = purchase(&app, &key, 1, 1_000).await;
    let clock = Arc::new(ShiftedClock::default());
    let deliverer = Deliverer::new(&app.state, clock.clone()).unwrap();
    let report = deliver_all(&deliverer).await;
    assert_eq!((report.delivered, report.retried), (0, 2), "{report:?}");
    let (_, events) = app.get(&format!("/v1/events?payment_id={pid}"), &key).await;
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["deliveries"][0][1] == "pending" && e["deliveries"][0][2] == 1)
    );

    let store = new_store();
    store
        .configure(StoreConfig {
            sandbox_url: api.url.clone(),
            api_key: reader,
            secrets: vec![secret],
        })
        .await;
    let _running = start_store(&addr, store.clone()).await;
    clock.jump(5_000);
    let report = deliver_all(&deliverer).await;
    assert_eq!(report.delivered, 2, "{report:?}");
    assert_eq!(store.fulfillments().await.unwrap().len(), 1);
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn webhook_targets_must_be_loopback_and_webhooks_need_a_server_key() {
    let (sim, app, key) = setup().await;
    for url in [
        "http://10.0.0.5:80/hook",
        "http://192.168.1.2:8080/hook",
        "http://169.254.169.254:80/latest/meta-data",
        "http://[fe80::1]:80/",
        "http://example.com:80/",
        "https://127.0.0.1:443/",
        "http://user:pw@127.0.0.1:80/",
    ] {
        let (status, reply) = app
            .post("/v1/webhook_endpoints", &key, None, json!({"url": url}))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{url}: {reply}");
    }
    let (_, reader) = app.key(&key, &["read"]).await;
    assert_eq!(
        app.post(
            "/v1/webhook_endpoints",
            &reader,
            None,
            json!({"url": "http://127.0.0.1:9/"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let off = TestApp::with_sim(&sim, &[]).await;
    let (_, off_key) = off.project("store").await;
    let (status, _) = off
        .post(
            "/v1/webhook_endpoints",
            &off_key,
            None,
            json!({"url": "http://127.0.0.1:9/"}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "no PS_SECRET_KEY, no webhooks"
    );
    off.cleanup().await;
    app.cleanup().await;
    sim.remove().await;
}

/// During a rotation both secrets sign, so a consumer holding either one keeps accepting.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rotation_keeps_consumers_on_either_secret_working() {
    let (sim, app, key) = setup().await;
    let (_, reader) = app.key(&key, &["read"]).await;
    let api = serve_api(&app).await;
    let store = new_store();
    let running = start_store("127.0.0.1:0", store.clone()).await;
    let (id, old) = endpoint(&app, &key, &format!("{}/webhooks", running.url)).await;
    let (status, rotated) = app
        .post(
            &format!("/v1/webhook_endpoints/{id}/rotate"),
            &key,
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let new = rotated["secret"].as_str().unwrap().to_owned();
    assert_ne!(new, old);
    let deliverer = Deliverer::new(&app.state, Arc::new(ShiftedClock::default())).unwrap();
    for (n, secrets) in [vec![new.clone()], vec![old.clone()]]
        .into_iter()
        .enumerate()
    {
        store
            .configure(StoreConfig {
                sandbox_url: api.url.clone(),
                api_key: reader.clone(),
                secrets,
            })
            .await;
        purchase(&app, &key, n, 0).await;
        let report = deliver_all(&deliverer).await;
        assert_eq!(
            (report.delivered, report.retried),
            (1, 0),
            "consumer {n}: {report:?}"
        );
    }
    app.cleanup().await;
    sim.remove().await;
}

fn signed(secret: &str, event: &EventPayload, timestamp: i64) -> (String, Vec<u8>) {
    let body = serde_json::to_vec(event).unwrap();
    (
        webhook::signature_header(&[secret], &event.id.to_string(), timestamp, &body),
        body,
    )
}

fn unix_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

async fn post_to(
    http: &reqwest::Client,
    url: &str,
    event: &EventPayload,
    header: &str,
    body: Vec<u8>,
) -> u16 {
    http.post(format!("{url}/webhooks"))
        .header(webhook::EVENT_ID_HEADER, event.id.to_string())
        .header(webhook::SIGNATURE_HEADER, header)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// The guide's failure case: duplicate or reordered webhooks. Every event of 30 payments is
/// delivered one to three times, in a random order, concurrently. The store ends with
/// exactly one fulfillment per payment that has captured money, and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[expect(
    clippy::too_many_lines,
    reason = "one fuzz scenario, read top to bottom"
)]
async fn duplicated_and_reordered_deliveries_fulfill_each_captured_payment_exactly_once() {
    let (sim, app, key) = setup().await;
    let (_, reader) = app.key(&key, &["read"]).await;
    let (_, project) = app.get("/v1/project", &key).await;
    let project_id: uuid::Uuid = project["project"]["id"].as_str().unwrap().parse().unwrap();
    let api = serve_api(&app).await;
    let store = new_store();
    let running = start_store("127.0.0.1:0", store.clone()).await;
    let (_, secret) = endpoint(&app, &key, &format!("{}/webhooks", running.url)).await;
    store
        .configure(StoreConfig {
            sandbox_url: api.url.clone(),
            api_key: reader,
            secrets: vec![secret.clone()],
        })
        .await;

    let mut seed: u64 = 0x5eed_cafe;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        seed >> 33
    };
    let mut with_captures = BTreeSet::new();
    for n in 0..30 {
        let action = next() % 4;
        let pid = purchase(
            &app,
            &key,
            n,
            if action == 0 {
                0
            } else {
                100 * i64::try_from(action).unwrap()
            },
        )
        .await;
        if action == 3 {
            app.post(
                &format!("/v1/payments/{pid}/refunds"),
                &key,
                Some(&format!("r{n}")),
                json!({"amount": 50}),
            )
            .await;
        }
        if action == 0 && next() % 2 == 0 {
            app.post(
                &format!("/v1/payments/{pid}/void"),
                &key,
                Some(&format!("v{n}")),
                json!({}),
            )
            .await;
        }
        if action > 0 {
            with_captures.insert(pid);
        }
    }
    let (_, listed) = app.get("/v1/events?limit=1000", &key).await;
    let events: Vec<EventPayload> = listed["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| EventPayload {
            id: e["id"].as_str().unwrap().parse().unwrap(),
            kind: e["kind"].as_str().unwrap().to_owned(),
            project_id,
            payment_id: e["payment_id"].as_str().unwrap().parse().unwrap(),
            operation_id: e["operation_id"].as_str().unwrap().parse().unwrap(),
            sequence: e["sequence"].as_i64().unwrap(),
            created_at: e["created_at"].as_i64().unwrap(),
            simulated: true,
        })
        .collect();
    let mut deliveries: Vec<EventPayload> = Vec::new();
    for event in &events {
        for _ in 0..=(next() % 3) {
            deliveries.push(event.clone());
        }
    }
    for i in (1..deliveries.len()).rev() {
        let j = usize::try_from(next()).unwrap() % (i + 1);
        deliveries.swap(i, j);
    }
    let http = reqwest::Client::new();
    let sent = deliveries.len();
    let mut tasks = tokio::task::JoinSet::new();
    let permits = Arc::new(tokio::sync::Semaphore::new(8));
    for event in deliveries {
        let (http, url, secret, permits) = (
            http.clone(),
            running.url.clone(),
            secret.clone(),
            permits.clone(),
        );
        tasks.spawn(async move {
            let _permit = permits.acquire_owned().await.unwrap();
            let (header, body) = signed(&secret, &event, unix_now());
            post_to(&http, &url, &event, &header, body).await
        });
    }
    while let Some(status) = tasks.join_next().await {
        assert_eq!(status.unwrap(), 200);
    }
    let fulfilled: BTreeSet<String> = store
        .fulfillments()
        .await
        .unwrap()
        .iter()
        .map(|f| f.payment_id.to_string())
        .collect();
    assert_eq!(
        fulfilled, with_captures,
        "exactly the payments with captured money, each once"
    );
    assert_eq!(
        usize::try_from(store.inbox_len().await.unwrap()).unwrap(),
        events.len()
    );
    assert!(sent > events.len(), "the fuzzer must have sent duplicates");
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forged_and_replayed_deliveries_are_refused_and_fulfill_nothing() {
    let (sim, app, key) = setup().await;
    let (_, reader) = app.key(&key, &["read"]).await;
    let api = serve_api(&app).await;
    let store = new_store();
    let running = start_store("127.0.0.1:0", store.clone()).await;
    let (_, secret) = endpoint(&app, &key, &format!("{}/webhooks", running.url)).await;
    store
        .configure(StoreConfig {
            sandbox_url: api.url.clone(),
            api_key: reader,
            secrets: vec![secret.clone()],
        })
        .await;
    let pid = purchase(&app, &key, 1, 500).await;
    let event = EventPayload {
        id: uuid::Uuid::now_v7(),
        kind: "capture.succeeded".to_owned(),
        project_id: uuid::Uuid::nil(),
        payment_id: pid.parse().unwrap(),
        operation_id: uuid::Uuid::now_v7(),
        sequence: 2,
        created_at: unix_now(),
        simulated: true,
    };
    let http = reqwest::Client::new();
    let forger = webhook::secret_from_bytes([9; 32]);
    let (header, body) = signed(&forger, &event, unix_now());
    assert_eq!(
        post_to(&http, &running.url, &event, &header, body).await,
        401,
        "forged"
    );
    let (header, body) = signed(&secret, &event, unix_now() - 400);
    assert_eq!(
        post_to(&http, &running.url, &event, &header, body).await,
        401,
        "replayed outside the window"
    );
    let (header, mut body) = signed(&secret, &event, unix_now());
    body.push(b' ');
    assert_eq!(
        post_to(&http, &running.url, &event, &header, body).await,
        401,
        "tampered"
    );
    assert_eq!(store.fulfillments().await.unwrap().len(), 0);
    app.cleanup().await;
    sim.remove().await;
}

/// Drill (b), the wall-clock half (D-026): one deliverer's attempt is slow; another
/// deliverer's clock jumps a minute ahead, sees the lease as expired, and delivers the same
/// event again. The receiver gets a duplicate — allowed, at-least-once — but the stale
/// worker's result is discarded by the fencing token: nothing is lost or corrupted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drill_b_a_lease_clock_jump_causes_at_most_a_duplicate_delivery() {
    let (sim, app, key) = setup().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let receiver = {
        let hits = hits.clone();
        axum::Router::new().route(
            "/hook",
            axum::routing::post(move || {
                let hits = hits.clone();
                async move {
                    if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(1_500)).await;
                    }
                    "ok"
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, receiver).await });
    endpoint(&app, &key, &format!("http://{addr}/hook")).await;
    purchase(&app, &key, 1, 0).await;

    let (clock_a, clock_b) = (
        Arc::new(ShiftedClock::default()),
        Arc::new(ShiftedClock::default()),
    );
    let a = Deliverer::new(&app.state, clock_a).unwrap();
    let b = Deliverer::new(&app.state, clock_b.clone()).unwrap();
    let first = tokio::spawn(async move { a.run_once().await.unwrap() });
    while hits.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    clock_b.jump(60_000);
    let second = b.run_once().await.unwrap();
    assert_eq!((second.claimed, second.delivered), (1, 1), "{second:?}");
    let first = first.await.unwrap();
    assert_eq!((first.claimed, first.stale), (1, 1), "{first:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 2, "one duplicate delivery");
    let row: (String, i32, Option<time::OffsetDateTime>) =
        sqlx::query_as("SELECT state, attempts, lease_until FROM deliveries")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!((row.0.as_str(), row.1, row.2), ("delivered", 1, None));
    app.cleanup().await;
    sim.remove().await;
}
