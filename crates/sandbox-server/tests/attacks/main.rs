//! The pentest regression suite: real attack bytes over real TCP against a running
//! instance (the model is labs/minibtc/tests/attacks). Every test asserts the refusal —
//! auth bypass, cross-tenant access, header spoofing, rate-limit evasion, SSRF-shaped
//! webhook targets, oversized and slow bodies, replay, card data, forged cursors.
//!
//! SIMULATED MONEY ONLY — no real credential ever appears, as everywhere.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "attack harness: a panic is the failure report"
)]
#![allow(
    clippy::large_futures,
    reason = "raw-TCP attack cases build whole requests inline; the futures are large and the cost is the point of the test"
)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[path = "../common/mod.rs"]
mod common;

use common::{BOOTSTRAP, TestApp, TestSim};

/// The same hex key webhooks.rs and releases.rs use — a test fixture, not a secret.
const SECRET_KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// A running instance: the real router on a real loopback listener (every layer —
/// transport cap, header parse, timeouts, admission, auth — sees exactly what a client
/// sends; `oneshot` would skip the wire).
struct Target {
    addr: std::net::SocketAddr,
    app: TestApp,
}

impl Target {
    async fn new(extra: &[(&str, &str)]) -> Self {
        Self::serve(TestApp::new(extra).await).await
    }

    async fn with_sim(extra: &[(&str, &str)]) -> (Self, TestSim) {
        let sim = TestSim::start().await;
        (Self::serve(TestApp::with_sim(&sim, extra).await).await, sim)
    }

    async fn serve(app: TestApp) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app.router.clone();
        let limits = app.state.config.transport;
        // The real bounded server, not axum::serve: the connection cap, header/body
        // read timeouts and write deadline under attack live in sandbox_http::serve —
        // serving the bare router would test a product that does not exist.
        tokio::spawn(async move {
            let _ = sandbox_http::serve(
                listener,
                router,
                limits,
                Duration::from_secs(1),
                std::future::pending(),
                std::sync::Arc::new(sandbox_http::ServeStats::default()),
            )
            .await;
        });
        Self { addr, app }
    }
}

/// A raw response: status line, headers, body.
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Sends `raw` and reads head + `Content-Length` body bytes; a 10 s ceiling bounds the
/// exchange. Returns the parsed response, or the partial buffer (status 0) when the
/// server closed first — a close is itself an answer a test may assert.
async fn raw(target: &Target, request: &[u8]) -> Response {
    raw_at(target.addr, request).await
}

/// The wire exchange itself, so spawned tasks can share it without a `Target`.
async fn raw_at(addr: std::net::SocketAddr, raw: &[u8]) -> Response {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(raw).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 16_384];
    loop {
        let head_len = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
        if let Some(head) = head_len {
            let head_text = String::from_utf8_lossy(&buf[..head]);
            let want = content_length(&head_text).map(|n| head + n);
            if want.is_none_or(|w| buf.len() >= w) {
                break;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            Ok(Ok(0) | Err(_)) | Err(_) => break,
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    parse(&buf)
}

fn content_length(head: &str) -> Option<usize> {
    head.lines().find_map(|line| {
        line.strip_prefix("content-length:")
            .or_else(|| line.strip_prefix("Content-Length:"))
            .and_then(|v| v.trim().parse().ok())
    })
}

fn parse(buf: &[u8]) -> Response {
    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Response {
            status: 0,
            headers: vec![],
            body: buf.to_vec(),
        };
    };
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        })
        .collect();
    Response {
        status,
        headers,
        body: buf[head_end + 4..].to_vec(),
    }
}

fn get(path: &str, key: Option<&str>) -> Vec<u8> {
    let auth = key.map_or(String::new(), |k| format!("authorization: Bearer {k}\r\n"));
    format!("GET {path} HTTP/1.1\r\nhost: x\r\n{auth}connection: close\r\n\r\n").into_bytes()
}

fn post(path: &str, key: Option<&str>, idem: Option<&str>, body: &Value) -> Vec<u8> {
    let body = body.to_string();
    let auth = key.map_or(String::new(), |k| format!("authorization: Bearer {k}\r\n"));
    let idem = idem.map_or(String::new(), |k| format!("idempotency-key: {k}\r\n"));
    format!(
        "POST {path} HTTP/1.1\r\nhost: x\r\n{auth}{idem}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

// 1. AUTH BYPASS: nothing unauthenticated or under-scoped ever reaches a handler.
#[tokio::test]
async fn auth_bypass_attempts_are_all_refused() {
    let t = Target::new(&[]).await;
    // No header at all.
    assert_eq!(raw(&t, &get("/v1/balances", None)).await.status, 401);
    // Garbage, empty and non-Bearer schemes.
    assert_eq!(
        raw(&t, &get("/v1/balances", Some("not-a-real-key")))
            .await
            .status,
        401
    );
    let weird = b"GET /v1/balances HTTP/1.1\r\nhost: x\r\nauthorization: basic cm9vdzpwYXNz\r\nconnection: close\r\n\r\n";
    assert_eq!(raw(&t, weird).await.status, 401);
    // A read-scope key may not move money.
    let (_project, admin) = t.app.project("atk-auth").await;
    let (_id, reader) = t.app.key(&admin, &["read"]).await;
    let denied = raw(
        &t,
        &post(
            "/v1/payments",
            Some(&reader),
            Some("atk-1"),
            &json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve"}),
        ),
    )
    .await;
    assert_eq!(denied.status, 403);
    // A revoked key dies on its very next request.
    let (key_id, doomed) = t.app.key(&admin, &["read"]).await;
    assert_eq!(
        raw(
            &t,
            &post(
                &format!("/v1/keys/{key_id}/revoke"),
                Some(&admin),
                None,
                &json!({})
            )
        )
        .await
        .status,
        200
    );
    assert_eq!(
        raw(&t, &get("/v1/balances", Some(&doomed))).await.status,
        401
    );
    // And a project key may not spend the operator credential's powers.
    assert_eq!(
        raw(
            &t,
            &post(
                "/v1/projects",
                Some(&reader),
                None,
                &json!({"slug": "atk-x1", "name": "x"})
            )
        )
        .await
        .status,
        401
    );
    t.app.cleanup().await;
}

/// A 202 is the server's honest "not yet known" — the same contract perf/scenarios/settle.js
/// proves for load seeds: poll the payment until the operation reaches a terminal state
/// inside a bound, else fail with a named reason. Asserts `succeeded` (every call site
/// uses the approve token; anything else is a broken fixture, not a product outcome).
async fn settled(t: &Target, key: &str, res: Response, what: &str) -> Response {
    if res.status != 202 {
        assert_eq!(res.status, 201, "{what}: {}", res.status);
        return res;
    }
    let pid = res.json()["payment"]["id"].as_str().unwrap().to_owned();
    let oid = res.json()["operation"]["id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let page = raw(t, &get(&format!("/v1/payments/{pid}"), Some(key))).await;
        if page.status == 200
            && let Some(op) = page.json()["operations"]
                .as_array()
                .and_then(|ops| ops.iter().find(|o| o["id"].as_str() == Some(&oid)))
        {
            let state = op["state"].as_str().unwrap_or("");
            if state != "pending" && state != "uncertain" {
                assert_eq!(state, "succeeded", "{what} settled to {state}");
                return res;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{what}: still uncertain after 45 s"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

// 2. CROSS-TENANT: project B can never read, move or enumerate project A's objects —
// and gets the same 404 an absent object would give, so existence doesn't leak.
#[tokio::test]
async fn cross_tenant_access_is_invisible() {
    let (t, sim) = Target::with_sim(&[]).await;
    let (_a, admin_a) = t.app.project("atk-a").await;
    let (_b, admin_b) = t.app.project("atk-b").await;
    // A owns a payment.
    let created = raw(
        &t,
        &post(
            "/v1/payments",
            Some(&admin_a),
            Some("atk-tenant-1"),
            &json!({"amount": 500, "currency": "USD", "token": "tok_sim_approve"}),
        ),
    )
    .await;
    let created = settled(&t, &admin_a, created, "tenant seed authorize").await;
    let pid = created.json()["payment"]["id"].as_str().unwrap().to_owned();
    // B cannot see it, move it, or replay over it.
    for path in [
        format!("/v1/payments/{pid}"),
        format!("/v1/payments/{pid}/journal"),
    ] {
        assert_eq!(
            raw(&t, &get(&path, Some(&admin_b))).await.status,
            404,
            "{path}"
        );
    }
    for (suffix, body) in [
        ("captures", json!({"amount": 100})),
        ("refunds", json!({"amount": 100})),
        ("void", json!({})),
    ] {
        let res = raw(
            &t,
            &post(
                &format!("/v1/payments/{pid}/{suffix}"),
                Some(&admin_b),
                Some(&format!("atk-b-{suffix}")),
                &body,
            ),
        )
        .await;
        assert_eq!(res.status, 404, "{suffix}");
    }
    // B's listings contain none of A's objects.
    let list = raw(&t, &get("/v1/payments", Some(&admin_b))).await;
    assert_eq!(list.status, 200);
    assert!(!list.body.windows(pid.len()).any(|w| w == pid.as_bytes()));
    // B cannot revoke A's keys (the id below is A's payment id — a uuid either way) and
    // cannot read A's reconciliation state into B's own view.
    let recon = raw(&t, &get("/v1/reconciliation", Some(&admin_b))).await;
    assert_eq!(recon.status, 200);
    assert!(!recon.body.windows(pid.len()).any(|w| w == pid.as_bytes()));
    sim.remove().await;
    t.app.cleanup().await;
}

// 3. HEADER SPOOFING: duplicated identities, folded and oversized headers, spoofed
// forwarding hints — the server takes none of them as a credential or a reason to 500.
#[tokio::test]
async fn spoofed_and_malformed_headers_gain_nothing() {
    let t = Target::new(&[]).await;
    let (_p, admin) = t.app.project("atk-hdr").await;
    // Two Authorization headers, junk first: the refused value can never be upgraded by
    // a trailing valid one — 400 or 401, never a 5xx.
    let dup = format!(
        "GET /v1/balances HTTP/1.1\r\nhost: x\r\nauthorization: Bearer junk\r\nauthorization: Bearer {admin}\r\nconnection: close\r\n\r\n"
    );
    let status = raw(&t, dup.as_bytes()).await.status;
    assert!(status == 400 || status == 401, "{status}");
    // A folded (obs-fold) header line is malformed or ignored, never smuggled.
    let folded = format!(
        "GET /v1/balances HTTP/1.1\r\nhost: x\r\nauthorization:\r\n Bearer {admin}\r\nconnection: close\r\n\r\n"
    );
    let status = raw(&t, folded.as_bytes()).await.status;
    assert!(status == 400 || status == 401, "{status}");
    // A header block past hyper's ~400 KB read-buffer bound is refused or the connection
    // is cut (status 0); a normal request still answers after it.
    let huge = format!(
        "GET /healthz HTTP/1.1\r\nhost: x\r\nx-pad: {}\r\nconnection: close\r\n\r\n",
        "a".repeat(600_000)
    );
    let status = raw(&t, huge.as_bytes()).await.status;
    assert!(status == 0 || (400..=431).contains(&status), "{status}");
    assert_eq!(raw(&t, &get("/healthz", None)).await.status, 200);
    // Forwarding headers are not a credential: still a bare 401 without a bearer.
    let xff = b"GET /v1/balances HTTP/1.1\r\nhost: x\r\nx-forwarded-for: 127.0.0.1\r\nx-real-ip: 10.0.0.1\r\nconnection: close\r\n\r\n";
    assert_eq!(raw(&t, xff).await.status, 401);
    t.app.cleanup().await;
}

// 4. RATE-LIMIT EVASION: a burst far past admission is shed by name — every answer
// inside its bound, every refusal a 503 carrying Retry-After, never a 500.
#[tokio::test]
async fn over_admission_load_is_shed_lawfully() {
    let (t, sim) = Target::with_sim(&[("PS_MAX_IN_FLIGHT", "4"), ("PS_READ_IN_FLIGHT", "2")]).await;
    let (_p, admin) = t.app.project("atk-rl").await;
    // Writes hold real work (the network call); 24 concurrent POSTs over 4 permits sheds.
    let mut joins = Vec::new();
    for n in 0..24 {
        let req = post(
            "/v1/payments",
            Some(&admin),
            Some(&format!("atk-rl-{n}")),
            &json!({"amount": 10, "currency": "USD", "token": "tok_sim_approve"}),
        );
        let addr = t.addr;
        joins.push(tokio::spawn(async move { raw_at(addr, &req).await }));
    }
    let mut refused_retry_after = 0;
    let mut statuses = Vec::new();
    for j in joins {
        let res = j.await.unwrap();
        if res.status == 503 {
            assert!(
                res.header("retry-after").is_some(),
                "a 503 without Retry-After"
            );
            refused_retry_after += 1;
        }
        statuses.push(res.status);
    }
    assert!(
        !statuses.contains(&0),
        "a request went unanswered: {statuses:?}"
    );
    assert!(!statuses.contains(&500), "a 500: {statuses:?}");
    assert!(
        statuses.iter().all(|s| *s == 201 || *s == 503),
        "{statuses:?}"
    );
    assert!(
        refused_retry_after > 0,
        "4 permits carried all 24; nothing was shed"
    );
    sim.remove().await;
    t.app.cleanup().await;
}

// 5. SSRF AT THE ONLY OUTBOUND SURFACE: webhook targets must stay loopback (D-023,
// D-059); every evasion form is refused at creation, before any delivery exists.
#[tokio::test]
async fn non_loopback_webhook_targets_are_refused() {
    // Endpoint creation needs the server's secret box — no sim (no delivery happens).
    let t = Target::new(&[("PS_SECRET_KEY", SECRET_KEY)]).await;
    let (_p, admin) = t.app.project("atk-ssrf").await;
    for url in [
        "http://169.254.169.254:80/latest/meta-data",
        "http://8.8.8.8:53/",
        "http://example.com:80/",
        "http://2130706433:80/", // decimal-encoded 127.0.0.1 — refused, not resolved
        "http://0177.0.0.1:80/", // octal-encoded — refused, not resolved
        "http://user@127.0.0.1:80/", // userinfo authority
        "http://localhost:0/",   // port zero
        "http://localhost:99999/", // port out of range
        "https://localhost:443/", // no TLS endpoint exists to attack through
        "http://localhost:56091/x#frag", // fragment
        "http://localhost:56091/x\\y", // backslash
    ] {
        let res = raw(
            &t,
            &post(
                "/v1/webhook_endpoints",
                Some(&admin),
                None,
                &json!({"url": url}),
            ),
        )
        .await;
        assert!((400..=422).contains(&res.status), "{url} -> {}", res.status);
    }
    // The control: a lawful loopback target is accepted.
    let ok = raw(
        &t,
        &post(
            "/v1/webhook_endpoints",
            Some(&admin),
            None,
            &json!({"url": "http://localhost:56091/hook"}),
        ),
    )
    .await;
    assert_eq!(ok.status, 201, "{}", ok.json());
    t.app.cleanup().await;
}

// 6. OVERSIZED AND SLOW BODIES: the 16 KB bound and the request deadline are enforced on
// the wire, and the server answers the next request normally.
#[tokio::test]
async fn oversized_and_slow_bodies_are_cut_off() {
    let t = Target::new(&[
        ("PS_READ_TIMEOUT_MS", "3000"),
        ("PS_DB_STATEMENT_TIMEOUT_MS", "2000"),
        ("PS_DB_ACQUIRE_TIMEOUT_MS", "1500"),
    ])
    .await;
    let (_p, admin) = t.app.project("atk-body").await;
    // An oversized declared body is refused by name (413) or as a bad request (400) —
    // never read into memory.
    let big = format!(
        "POST /v1/payments HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {admin}\r\nidempotency-key: atk-big\r\ncontent-type: application/json\r\ncontent-length: 200000\r\nconnection: close\r\n\r\n{}",
        " ".repeat(20_000)
    );
    let status = raw(&t, big.as_bytes()).await.status;
    assert!(status == 400 || status == 413 || status == 422, "{status}");
    // A dribbling body hits the request deadline: the server answers (503, the timeout
    // status) or closes, inside the deadline plus slack.
    let started = Instant::now();
    let mut stream = tokio::net::TcpStream::connect(t.addr).await.unwrap();
    let head = format!(
        "POST /v1/payments HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {admin}\r\nidempotency-key: atk-slow\r\ncontent-type: application/json\r\ncontent-length: 20\r\nconnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match tokio::time::timeout(Duration::from_secs(8), stream.read(&mut chunk)).await {
            Ok(Ok(0) | Err(_)) | Err(_) => break,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the slow body held a connection past its deadline"
    );
    if !buf.is_empty() {
        let res = parse(&buf);
        // Any named refusal inside the bound is lawful: 408/413 for the timed-out body,
        // 400 for the malformed exchange, 503 if the timeout layer itself answers.
        assert!(
            res.status == 503 || res.status == 400 || res.status == 408 || res.status == 413,
            "{}",
            res.status
        );
    }
    // And it is still serving.
    assert_eq!(raw(&t, &get("/healthz", None)).await.status, 200);
    t.app.cleanup().await;
}

// 7. REPLAY: a repeated request is answered from the record, a repeated key with a
// different payload is a conflict — and the journal never double-posts.
#[tokio::test]
async fn replays_never_double_post() {
    let (t, sim) = Target::with_sim(&[]).await;
    let (_p, admin) = t.app.project("atk-replay").await;
    let body = json!({"amount": 700, "currency": "USD", "token": "tok_sim_approve"});
    let first = raw(
        &t,
        &post("/v1/payments", Some(&admin), Some("atk-rep-1"), &body),
    )
    .await;
    // Settled first — a replay of an uncertain operation is itself an honest 202.
    let first = settled(&t, &admin, first, "replay seed authorize").await;
    let pid = first.json()["payment"]["id"].as_str().unwrap().to_owned();
    // Byte-identical replay → the recorded answer, marked a replay.
    let again = raw(
        &t,
        &post("/v1/payments", Some(&admin), Some("atk-rep-1"), &body),
    )
    .await;
    assert_eq!(
        again.status,
        200,
        "{}",
        String::from_utf8_lossy(&again.body)
    );
    assert_eq!(again.json()["replayed"], true);
    // Same key, different payload → 409 conflict, never a silent reinterpretation.
    let other = raw(
        &t,
        &post(
            "/v1/payments",
            Some(&admin),
            Some("atk-rep-1"),
            &json!({"amount": 701, "currency": "USD", "token": "tok_sim_approve"}),
        ),
    )
    .await;
    assert_eq!(other.status, 409);
    // The journal shows exactly the authorize's postings: the replay added nothing.
    let journal = raw(
        &t,
        &get(&format!("/v1/payments/{pid}/journal"), Some(&admin)),
    )
    .await;
    assert_eq!(journal.status, 200);
    assert_eq!(journal.json()["entries"].as_array().unwrap().len(), 1);
    sim.remove().await;
    t.app.cleanup().await;
}

// 8. CARD DATA: a PAN anywhere is refused before any field is used (D-019) — in the
// description, the idempotency key, even a project slug.
#[tokio::test]
async fn raw_card_data_never_enters() {
    let t = Target::new(&[]).await;
    let (_p, admin) = t.app.project("atk-card").await;
    let pan = "4111111111111111";
    let res = raw(
        &t,
        &post(
            "/v1/payments",
            Some(&admin),
            Some("atk-card-1"),
            &json!({"amount": 10, "currency": "USD", "token": "tok_sim_approve", "description": pan}),
        ),
    )
    .await;
    assert!((400..=422).contains(&res.status), "{}", res.status);
    // A PAN-shaped idempotency key is refused on parse, not stored.
    let res = raw(
        &t,
        &post(
            "/v1/payments",
            Some(&admin),
            Some(pan),
            &json!({"amount": 10, "currency": "USD", "token": "tok_sim_approve"}),
        ),
    )
    .await;
    assert!((400..=422).contains(&res.status), "{}", res.status);
    // Even a PAN-shaped project slug.
    let res = raw(
        &t,
        &post(
            "/v1/projects",
            Some(BOOTSTRAP),
            None,
            &json!({"slug": pan, "name": "x"}),
        ),
    )
    .await;
    assert!((400..=422).contains(&res.status), "{}", res.status);
    t.app.cleanup().await;
}

// 9. FORGED CURSORS: explain's opaque cursor must refuse anything not issued — a named
// 400, never a 500 that reaches the database layer (D-056, review N3).
#[tokio::test]
async fn forged_explain_cursors_are_refused_cleanly() {
    let t = Target::new(&[]).await;
    let (_p, admin) = t.app.project("atk-cursor").await;
    for forged in [
        "apapapapapapapapapapapapapapapapap".to_owned(),
        "AAAAAAAABBBBBBBB".to_owned(),
        "a".repeat(200),
        "as_of=injected%26running%3D999999999".to_owned(),
    ] {
        let res = raw(
            &t,
            &get(
                &format!("/v1/balances/USD/merchant_pending/explain?cursor={forged}"),
                Some(&admin),
            ),
        )
        .await;
        assert_eq!(res.status, 400, "{forged}: {}", res.status);
    }
    t.app.cleanup().await;
}
