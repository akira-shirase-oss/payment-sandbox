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
    // The server may cut an over-bounded head while the request is still being written:
    // the close is the answer, so a refused write does not panic — read what came back.
    let _ = stream.write_all(raw).await;
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
    // A revoked key dies on its very next request — and it must be in the cache when
    // revoked, or the test proves nothing about invalidation: a key never resolved would
    // fail the SELECT the same way with the whole cache machinery deleted.
    let (key_id, doomed) = t.app.key(&admin, &["read"]).await;
    assert_eq!(
        raw(&t, &get("/v1/balances", Some(&doomed))).await.status,
        200,
        "seed the key cache before the revoke"
    );
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
    // Stored state: nothing the bypass attempts touched was persisted.
    let payments = raw(&t, &get("/v1/payments?limit=500", Some(&admin))).await;
    assert_eq!(payments.json()["payments"].as_array().unwrap().len(), 0);
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

/// Posts an approve-token authorize and waits for it to settle — the seed step the
/// tenant tests need. Returns the payment's id.
async fn authorize(t: &Target, admin: &str, idem: &str, amount: i64) -> String {
    let created = raw(
        t,
        &post(
            "/v1/payments",
            Some(admin),
            Some(idem),
            &json!({"amount": amount, "currency": "USD", "token": "tok_sim_approve"}),
        ),
    )
    .await;
    settled(t, admin, created, "seed authorize").await.json()["payment"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

// 2. CROSS-TENANT: project B can never read, move or enumerate project A's objects —
// and gets the same 404 an absent object would give, so existence doesn't leak.
#[tokio::test]
async fn cross_tenant_access_is_invisible() {
    let (t, sim) = Target::with_sim(&[("PS_SECRET_KEY", SECRET_KEY)]).await;
    let (_a, admin_a) = t.app.project("atk-a").await;
    let (_b, admin_b) = t.app.project("atk-b").await;
    // A owns a payment.
    let pid = authorize(&t, &admin_a, "atk-tenant-1", 500).await;
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
    // B cannot revoke A's keys: a real key of A's, guessed by id, answers the same 404
    // an absent one would.
    let (a_key_id, _a_key) = t.app.key(&admin_a, &["read"]).await;
    assert_eq!(
        raw(
            &t,
            &post(
                &format!("/v1/keys/{a_key_id}/revoke"),
                Some(&admin_b),
                None,
                &json!({})
            )
        )
        .await
        .status,
        404
    );
    // A's webhook endpoints are invisible to B's listing.
    let endpoint = raw(
        &t,
        &post(
            "/v1/webhook_endpoints",
            Some(&admin_a),
            None,
            &json!({"url": "http://localhost:56091/hook"}),
        ),
    )
    .await;
    assert_eq!(
        endpoint.status,
        201,
        "{}",
        String::from_utf8_lossy(&endpoint.body)
    );
    let b_endpoints = raw(&t, &get("/v1/webhook_endpoints", Some(&admin_b))).await;
    assert_eq!(
        b_endpoints.json()["endpoints"].as_array().unwrap().len(),
        0,
        "B saw A's webhook endpoints"
    );
    // B owns one payment of its own, so B's reconciliation actually ran work — and it
    // still reports nothing of A's: the check is only meaningful when it counted B's.
    authorize(&t, &admin_b, "atk-tenant-b1", 50).await;
    let recon = raw(&t, &get("/v1/reconciliation", Some(&admin_b))).await;
    assert_eq!(recon.status, 200);
    let checked = recon.json()["report"]["payments_checked"]
        .as_i64()
        .unwrap_or(-1);
    assert!(
        checked >= 1,
        "B's reconciliation counted nothing: {checked}"
    );
    assert!(!recon.body.windows(pid.len()).any(|w| w == pid.as_bytes()));
    // Stored state: A's payment is intact and still A's — B's attempts changed nothing.
    let reread = raw(&t, &get(&format!("/v1/payments/{pid}"), Some(&admin_a))).await;
    assert_eq!(reread.status, 200);
    assert_eq!(reread.json()["payment"]["id"].as_str().unwrap(), pid);
    assert_eq!(reread.json()["payment"]["amount"], 500);
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
    // Inside the 32 KiB head bound a header block still answers — the bound protects
    // memory, not size for its own sake.
    let inside = format!(
        "GET /healthz HTTP/1.1\r\nhost: x\r\nx-pad: {}\r\nconnection: close\r\n\r\n",
        "a".repeat(16 * 1024)
    );
    assert_eq!(raw(&t, inside.as_bytes()).await.status, 200);
    // Past it the head is refused by name (431); past the kernel's delivery window the
    // refusal may arrive as a cut instead (status 0) — never as an answer.
    let over = format!(
        "GET /healthz HTTP/1.1\r\nhost: x\r\nx-pad: {}\r\nconnection: close\r\n\r\n",
        "a".repeat(64 * 1024)
    );
    let status = raw(&t, over.as_bytes()).await.status;
    assert!(status == 431 || status == 0, "{status}");
    let huge = format!(
        "GET /healthz HTTP/1.1\r\nhost: x\r\nx-pad: {}\r\nconnection: close\r\n\r\n",
        "a".repeat(600_000)
    );
    let status = raw(&t, huge.as_bytes()).await.status;
    assert!(status == 0 || status == 431, "{status}");
    assert_eq!(raw(&t, &get("/healthz", None)).await.status, 200);
    // Forwarding headers are not a credential: still a bare 401 without a bearer.
    let xff = b"GET /v1/balances HTTP/1.1\r\nhost: x\r\nx-forwarded-for: 127.0.0.1\r\nx-real-ip: 10.0.0.1\r\nconnection: close\r\n\r\n";
    assert_eq!(raw(&t, xff).await.status, 401);
    // Stored state: no smuggled request created anything.
    let payments = raw(&t, &get("/v1/payments?limit=500", Some(&admin))).await;
    assert_eq!(payments.json()["payments"].as_array().unwrap().len(), 0);
    t.app.cleanup().await;
}

// 4. RATE-LIMIT EVASION: a burst far past admission is shed by name — every answer
// inside its bound, every refusal a 503 carrying Retry-After, never a 500.
#[tokio::test]
async fn over_admission_load_is_shed_lawfully() {
    let (t, sim) = Target::with_sim(&[("PS_MAX_IN_FLIGHT", "4"), ("PS_READ_IN_FLIGHT", "2")]).await;
    let (_p, admin) = t.app.project("atk-rl").await;
    // Every authorize holds its admission slot for 800 ms of simulated network latency,
    // so more than 4 of 24 concurrent POSTs shed at machine speed, not by luck.
    sim.fault(json!({"kind": "authorize", "target": null, "action": "delay", "delay_ms": 800, "count": 24}));
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
            // The refusal must be the admission shed itself — a pool-deadline or a
            // payment-busy 503 would pass a bare status check while proving nothing.
            assert_eq!(
                res.json()["error"]["code"].as_str(),
                Some("overloaded"),
                "a 503 that is not the named admission shed: {}",
                String::from_utf8_lossy(&res.body)
            );
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
        refused_retry_after > 4,
        "at most 4 permits for 24 concurrent requests, yet only {refused_retry_after} shed"
    );
    // Stored state: exactly the admitted requests persisted — nothing half-wrote.
    let created = statuses.iter().filter(|s| **s == 201).count();
    let payments = raw(&t, &get("/v1/payments?limit=500", Some(&admin))).await;
    assert_eq!(
        payments.json()["payments"].as_array().unwrap().len(),
        created
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
    // Every evasion form is refused the same way — the named invalid-body 422, never a
    // status that could hide a different failure.
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
        "http://0.0.0.0:80/",    // the unspecified address
        "http://0:80/",          // its shortest form
        "http://[fe80::1]:80/",  // IPv6 link-local
        "http://[2001:db8::1]:443/", // IPv6 global unicast
        "http://[64:ff9b::7f00:1]:80/", // NAT64 wrapping a v4 — not the mapped form
        "http://[::ffff:8.8.8.8]:80/", // v4-mapped v6, non-loopback
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
        assert_eq!(res.status, 422, "{url} -> {}", res.status);
    }
    // The controls: lawful loopback targets are accepted, on both address families.
    for url in ["http://localhost:56091/hook", "http://[::1]:56091/hook"] {
        let ok = raw(
            &t,
            &post(
                "/v1/webhook_endpoints",
                Some(&admin),
                None,
                &json!({"url": url}),
            ),
        )
        .await;
        assert_eq!(ok.status, 201, "{url}: {}", ok.json());
    }
    // Stored state: only the lawful control endpoints persisted; every refused URL
    // left nothing behind for a delivery worker to find later.
    let endpoints = raw(&t, &get("/v1/webhook_endpoints", Some(&admin))).await;
    assert_eq!(endpoints.status, 200);
    assert_eq!(endpoints.json()["endpoints"].as_array().unwrap().len(), 2);
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
    // A body that trickles real bytes forever never completes: the body deadline cuts it
    // off by name, inside the deadline plus slack. (Sending nothing at all would test a
    // stalled read, not a dribble — a silent close would pass a status check that never
    // saw one.)
    let started = Instant::now();
    let mut stream = tokio::net::TcpStream::connect(t.addr).await.unwrap();
    // 40 declared, at most 20 ever sent — the dribble can never complete; what kills it
    // is the stall after the last byte, not a finished body failing to parse.
    let head = format!(
        "POST /v1/payments HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {admin}\r\nidempotency-key: atk-slow\r\ncontent-type: application/json\r\ncontent-length: 40\r\nconnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut tick = tokio::time::interval(Duration::from_millis(400));
    let mut wrote = 0_usize;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if wrote < 16 {
                    let _ = stream.write_all(b"{\"a\":").await;
                    wrote += 5;
                }
            }
            read = tokio::time::timeout(Duration::from_secs(8), stream.read(&mut chunk)) => {
                match read {
                    Ok(Ok(0) | Err(_)) | Err(_) => break,
                    Ok(Ok(n)) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
        }
        if started.elapsed() >= Duration::from_secs(8) {
            break;
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the dribbled body held a connection past its deadline"
    );
    assert!(
        !buf.is_empty(),
        "the dribbled body was dropped with no answer"
    );
    let res = parse(&buf);
    // The named refusal for a body out of time: 408 `body_timeout`, its own answer —
    // not 413, which names a body that was too big (nothing here was).
    assert_eq!(res.status, 408, "{}", res.status);
    assert_eq!(
        res.json()["error"]["code"].as_str(),
        Some("body_timeout"),
        "{:?}",
        String::from_utf8_lossy(&res.body)
    );
    // And it is still serving.
    assert_eq!(raw(&t, &get("/healthz", None)).await.status, 200);
    // Stored state: neither the oversized nor the dribbled body persisted a payment.
    let payments = raw(&t, &get("/v1/payments?limit=500", Some(&admin))).await;
    assert_eq!(payments.json()["payments"].as_array().unwrap().len(), 0);
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
    // Stored state: the list still shows exactly one payment — the conflicting
    // retry created no second object.
    let payments = raw(&t, &get("/v1/payments?limit=500", Some(&admin))).await;
    assert_eq!(payments.json()["payments"].as_array().unwrap().len(), 1);
    sim.remove().await;
    t.app.cleanup().await;
}

// 8. CARD DATA: a PAN anywhere is refused before any field is used (D-019) — in the
// description, the idempotency key, even a project slug.
#[tokio::test]
async fn raw_card_data_never_enters() {
    let (t, sim) = Target::with_sim(&[]).await;
    let (_p, admin) = t.app.project("atk-card").await;
    let pan = "4111111111111111";
    // Every PAN surface is refused the same way — by the named guard, not by any
    // client-error status: a 400 from an unrelated parse failure would pass the old
    // range assert while proving nothing about the card guard.
    for (path, key, idem, body) in [
        (
            "/v1/payments",
            Some(admin.as_str()),
            Some("atk-card-1"),
            json!({"amount": 10, "currency": "USD", "token": "tok_sim_approve", "description": pan}),
        ),
        (
            "/v1/payments",
            Some(admin.as_str()),
            Some(pan),
            json!({"amount": 10, "currency": "USD", "token": "tok_sim_approve"}),
        ),
        (
            "/v1/projects",
            Some(BOOTSTRAP),
            None,
            json!({"slug": pan, "name": "x"}),
        ),
    ] {
        let res = raw(&t, &post(path, key, idem, &body)).await;
        assert_eq!(res.status, 422, "{body}");
        assert_eq!(
            res.json()["error"]["code"].as_str(),
            Some("raw_card_data_rejected"),
            "{body} was not refused by the card guard: {}",
            String::from_utf8_lossy(&res.body)
        );
    }
    // The positive control: the same payment without the PAN succeeds — the refusals
    // above were the guard's, not a broken route.
    let ok = raw(
        &t,
        &post(
            "/v1/payments",
            Some(&admin),
            Some("atk-card-ok"),
            &json!({"amount": 10, "currency": "USD", "token": "tok_sim_approve", "description": "a lawful description"}),
        ),
    )
    .await;
    settled(&t, &admin, ok, "card control authorize").await;
    // Stored state: exactly the control payment exists, and the PAN string is in no
    // stored record a reader could enumerate.
    let payments = raw(&t, &get("/v1/payments?limit=500", Some(&admin))).await;
    assert_eq!(payments.json()["payments"].as_array().unwrap().len(), 1);
    assert!(
        !payments
            .body
            .windows(pan.len())
            .any(|w| w == pan.as_bytes()),
        "a PAN-shaped string persisted"
    );
    sim.remove().await;
    t.app.cleanup().await;
}

// 9. FORGED CURSORS: explain's opaque cursor must refuse anything not issued — a named
// 400, never a 500 that reaches the database layer (D-056, review N3).
#[tokio::test]
async fn forged_explain_cursors_are_refused_cleanly() {
    let (t, sim) = Target::with_sim(&[]).await;
    let (_p, admin) = t.app.project("atk-cursor").await;
    // Two captured payments so `explain?limit=1` answers with a real sealed cursor in
    // `next` — the well-formed forgeries below are built on it.
    for idem in ["atk-cur-1", "atk-cur-2"] {
        let created = raw(
            &t,
            &post(
                "/v1/payments",
                Some(&admin),
                Some(idem),
                &json!({"amount": 100, "currency": "USD", "token": "tok_sim_approve"}),
            ),
        )
        .await;
        let created = settled(&t, &admin, created, "cursor seed authorize").await;
        let pid = created.json()["payment"]["id"].as_str().unwrap().to_owned();
        let captured = raw(
            &t,
            &post(
                &format!("/v1/payments/{pid}/captures"),
                Some(&admin),
                Some(&format!("{idem}-cap")),
                &json!({"amount": 100}),
            ),
        )
        .await;
        settled(&t, &admin, captured, "cursor seed capture").await;
    }
    let explain = "/v1/balances/USD/merchant_pending/explain";
    let page = raw(&t, &get(&format!("{explain}?limit=1"), Some(&admin))).await;
    assert_eq!(page.status, 200, "{}", String::from_utf8_lossy(&page.body));
    let issued = page.json()["explanation"]["next"]["cursor"]
        .as_str()
        .expect("two captured payments must leave a second page")
        .to_owned();
    // Malformed blobs die at shape, decode or field grammar — before the tag is read.
    for forged in [
        "apapapapapapapapapapapapapapapapap".to_owned(),
        "AAAAAAAABBBBBBBB".to_owned(),
        "a".repeat(200),
        "as_of=injected%26running%3D999999999".to_owned(),
    ] {
        let res = raw(
            &t,
            &get(&format!("{explain}?cursor={forged}"), Some(&admin)),
        )
        .await;
        assert_eq!(res.status, 400, "{forged}: {}", res.status);
    }
    // The case the tag itself must catch — both still decode and parse; only the tag is
    // wrong. (a) Flip one letter inside the issued cursor's tag tail.
    let mut flipped: Vec<char> = issued.chars().collect();
    let last = flipped.len() - 1;
    flipped[last] = if flipped[last] == 'a' { 'b' } else { 'a' };
    // (b) The attacker's edit the seal exists to stop: the payload's running total
    // rewritten, re-encoded under the original tag.
    let bytes = sandbox_domain::letters::decode(&issued).unwrap();
    let (payload, tag) = bytes.split_at(bytes.len() - 16);
    let text = std::str::from_utf8(payload).unwrap().to_owned();
    let edited = format!("{}|999999999", text.rsplit_once('|').unwrap().0);
    let mut resealed = sandbox_domain::letters::encode(edited.as_bytes());
    resealed.push_str(&sandbox_domain::letters::encode(tag));
    for forged in [flipped.into_iter().collect::<String>(), resealed] {
        let res = raw(
            &t,
            &get(&format!("{explain}?cursor={forged}"), Some(&admin)),
        )
        .await;
        assert_eq!(
            res.status, 400,
            "a well-formed forgery served: {}",
            res.status
        );
    }
    // A cursor issued for this account explains nothing else: replay it against
    // payer_held and the context binding refuses it before the tag is even compared.
    let res = raw(
        &t,
        &get(
            &format!("/v1/balances/USD/payer_held/explain?cursor={issued}"),
            Some(&admin),
        ),
    )
    .await;
    assert_eq!(res.status, 400);
    // And replayed under another project it is the same refusal: the seal binds the
    // project, so a cursor A holds is not a read oracle for B.
    let (_pb, admin_b) = t.app.project("atk-cursor-b").await;
    let res = raw(
        &t,
        &get(&format!("{explain}?cursor={issued}"), Some(&admin_b)),
    )
    .await;
    assert_eq!(res.status, 400, "A's cursor served under B's key");
    // Stored state: the endpoint still works for an honestly-issued (cursor-free) read.
    let clean = raw(&t, &get(explain, Some(&admin))).await;
    assert_eq!(clean.status, 200);
    sim.remove().await;
    t.app.cleanup().await;
}
