//! The hosted checkout (D-060, D-061, D-062): sessions created by the merchant, paid through
//! the checkout origin's own API by a page that holds only the session's secret, and never
//! charged twice: the same key re-drives its attempt, another key waits for the last
//! attempt to end without money, and "paid" comes only from captured money.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use axum::Router;
use axum::http::{Method, StatusCode};
use common::{TestApp, TestSim, raw, test_site};
use sandbox_server::reconciler;
use serde_json::{Value, json};
use sha2::Digest as _;

const ORIGIN: &str = "http://127.0.0.1:56083";

struct Page {
    router: Router,
    dir: std::path::PathBuf,
}

impl Page {
    fn new(app: &TestApp) -> Self {
        let (dir, site) = test_site();
        Self {
            router: sandbox_server::web::checkout::router(&app.state, site, ORIGIN),
            dir,
        }
    }

    async fn view(&self, id: &str, secret: &str) -> (StatusCode, Value) {
        let bearer = format!("Bearer {secret}");
        let (status, _, body) = raw(
            &self.router,
            Method::GET,
            &format!("/api/sessions/{id}"),
            &[("authorization", &bearer)],
            None,
        )
        .await;
        (status, body)
    }

    async fn pay(&self, id: &str, secret: &str, key: &str, token: &str) -> (StatusCode, Value) {
        let bearer = format!("Bearer {secret}");
        let (status, _, body) = raw(
            &self.router,
            Method::POST,
            &format!("/api/sessions/{id}/attempts"),
            &[
                ("authorization", &bearer),
                ("origin", ORIGIN),
                ("sec-fetch-site", "same-origin"),
                ("idempotency-key", key),
            ],
            Some(json!({"token": token})),
        )
        .await;
        (status, body)
    }

    async fn resume(&self, id: &str, secret: &str) -> (StatusCode, Value) {
        let bearer = format!("Bearer {secret}");
        let (status, _, body) = raw(
            &self.router,
            Method::POST,
            &format!("/api/sessions/{id}/continue"),
            &[("authorization", &bearer), ("origin", ORIGIN)],
            Some(json!({})),
        )
        .await;
        (status, body)
    }

    fn remove(self) {
        let _ = std::fs::remove_dir_all(self.dir);
    }
}

/// Creates a session; returns (id, client secret, the whole answer).
async fn session(app: &TestApp, key: &str, body: Value) -> (String, String, Value) {
    let (status, created) = app.post("/v1/checkout_sessions", key, None, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    (
        created["session"]["id"].as_str().unwrap().to_owned(),
        created["client_secret"].as_str().unwrap().to_owned(),
        created,
    )
}

fn state(view: &Value) -> (&str, Option<&str>) {
    (
        view["session"]["state"].as_str().unwrap(),
        view["session"]["next_action"].as_str(),
    )
}

#[tokio::test]
async fn a_session_is_paid_once_and_a_retry_never_charges_twice() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("shop").await;
    let page = Page::new(&app);
    let (id, secret, created) = session(
        &app,
        &key,
        json!({"amount": 1_250, "currency": "USD", "description": "Order 7 of the dev store"}),
    )
    .await;
    // The payer's URL carries the secret in its fragment, which browsers never send.
    assert_eq!(
        created["url"].as_str().unwrap(),
        format!("{ORIGIN}/c/{id}#{secret}")
    );
    assert!(secret.starts_with("cs_sim_") && !secret.bytes().any(|b| b.is_ascii_digit()));

    // Without the secret, or with another, the session is invisible.
    let (status, _) = page.view(&id, "cs_sim_nope").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let other = format!("cs_sim_{}", "a".repeat(64));
    assert_eq!(page.view(&id, &other).await.0, StatusCode::UNAUTHORIZED);
    let (status, view) = page.view(&id, &secret).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(state(&view), ("open", Some("pay")));
    assert_eq!(view["session"]["amount_display"], "12.50");
    assert_eq!(view["session"]["merchant"], "shop");
    assert_eq!(view["scenarios"].as_array().unwrap().len(), 6);
    let text = view.to_string();
    for absent in [
        "client_secret",
        "\"number\"",
        "cvc",
        "secret_hash",
        "idempotency",
    ] {
        assert!(!text.contains(absent), "{absent} in {text}");
    }

    let key_1 = "0b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001";
    let (status, paid) = page.pay(&id, &secret, key_1, "tok_sim_approve").await;
    assert_eq!(status, StatusCode::OK, "{paid}");
    assert_eq!(state(&paid), ("paid", None));
    assert_eq!(paid["captured"], 1_250);
    // A double click or a retry with the same key: the same attempt, nothing more.
    let (status, again) = page.pay(&id, &secret, key_1, "tok_sim_approve").await;
    assert_eq!((status, state(&again)), (StatusCode::OK, ("paid", None)));
    assert_eq!(again["attempt"]["number"], 1);
    // Another key cannot start a second charge; the same key with another scenario is a
    // conflict, never a reinterpretation.
    let (status, open) = page
        .pay(
            &id,
            &secret,
            "0b1e6c8a-7f42-4c1e-9d3a-5be1f00d0002",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{open}");
    assert_eq!(open["error"]["code"], "checkout_attempt_open");
    let (status, conflict) = page
        .pay(&id, &secret, key_1, "tok_sim_decline_do_not_honor")
        .await;
    assert_eq!(
        conflict["error"]["code"], "idempotency_key_conflict",
        "{status}"
    );
    assert_eq!(
        sim.store.count().await.unwrap(),
        2,
        "one authorization and one capture at the network"
    );

    let (_, merchant) = app.get(&format!("/v1/checkout_sessions/{id}"), &key).await;
    let attempts = merchant["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1, "{merchant}");
    let payment_id = attempts[0]["payment_id"].as_str().unwrap();
    let (_, payment) = app.get(&format!("/v1/payments/{payment_id}"), &key).await;
    assert_eq!(payment["payment"]["status"], "captured", "{payment}");
    assert_eq!(
        payment["payment"]["description"],
        "Order 7 of the dev store"
    );
    let (_, list) = app.get("/v1/payments", &key).await;
    assert_eq!(list["payments"].as_array().unwrap().len(), 1);
    app.assert_reconciled(&key).await;
    page.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn a_decline_lets_the_payer_try_again_and_only_the_approval_charges() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("shop").await;
    let page = Page::new(&app);
    let (id, secret, _) = session(&app, &key, json!({"amount": 900, "currency": "JPY"})).await;
    let (_, declined) = page
        .pay(
            &id,
            &secret,
            "1b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001",
            "tok_sim_decline_insufficient_funds",
        )
        .await;
    assert_eq!(state(&declined), ("declined", Some("pay")), "{declined}");
    assert_eq!(declined["attempt"]["decline_code"], "insufficient_funds");
    assert_eq!(declined["captured"], 0);
    let (_, paid) = page
        .pay(
            &id,
            &secret,
            "1b1e6c8a-7f42-4c1e-9d3a-5be1f00d0002",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(state(&paid), ("paid", None), "{paid}");
    assert_eq!(paid["attempt"]["number"], 2);
    assert_eq!(paid["session"]["amount_display"], "900");
    let (_, merchant) = app.get(&format!("/v1/checkout_sessions/{id}"), &key).await;
    assert_eq!(merchant["attempts"].as_array().unwrap().len(), 2);
    let (_, list) = app.get("/v1/payments", &key).await;
    let statuses: Vec<&str> = list["payments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses.iter().filter(|s| **s == "captured").count(),
        1,
        "{list}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == "declined").count(),
        1,
        "{list}"
    );
    app.assert_reconciled(&key).await;
    page.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn an_uncertain_answer_is_never_shown_as_paid_until_readback_resolves_it() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[("PS_SIM_TIMEOUT_MS", "300")]).await;
    let (_, key) = app.project("shop").await;
    let page = Page::new(&app);
    let (id, secret, _) = session(&app, &key, json!({"amount": 4_000, "currency": "EUR"})).await;
    let pay_key = "2b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001";
    let (status, lost) = page.pay(&id, &secret, pay_key, "tok_sim_lost_reply").await;
    assert_eq!(status, StatusCode::OK, "{lost}");
    assert_eq!(state(&lost), ("uncertain", Some("wait")), "{lost}");
    assert_eq!(lost["attempt"]["authorize"], "uncertain");
    // Waiting changes nothing; a new attempt is refused while the answer is unknown.
    assert_eq!(
        state(&page.view(&id, &secret).await.1),
        ("uncertain", Some("wait"))
    );
    let (status, refused) = page
        .pay(
            &id,
            &secret,
            "2b1e6c8a-7f42-4c1e-9d3a-5be1f00d0002",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(
        (status, refused["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("checkout_attempt_open"))
    );

    // The reconciler reads the network's record back under the same identity.
    reconciler::sweep(&app.state).await.unwrap();
    let (_, resolved) = page.view(&id, &secret).await;
    assert_eq!(
        state(&resolved),
        ("authorized", Some("continue")),
        "{resolved}"
    );
    let (_, paid) = page.resume(&id, &secret).await;
    assert_eq!(state(&paid), ("paid", None), "{paid}");
    assert_eq!(
        sim.store.count().await.unwrap(),
        2,
        "authorized once, captured once"
    );
    app.assert_reconciled(&key).await;

    // The network that never executed: uncertain, then "not charged", then a new attempt.
    let (id, secret, _) = session(&app, &key, json!({"amount": 500, "currency": "USD"})).await;
    let (_, unavailable) = page
        .pay(
            &id,
            &secret,
            "3b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001",
            "tok_sim_unavailable",
        )
        .await;
    assert_eq!(
        state(&unavailable),
        ("uncertain", Some("wait")),
        "{unavailable}"
    );
    reconciler::sweep(&app.state).await.unwrap();
    let (_, fenced) = page.view(&id, &secret).await;
    assert_eq!(state(&fenced), ("not_executed", Some("pay")), "{fenced}");
    let (_, paid) = page
        .pay(
            &id,
            &secret,
            "3b1e6c8a-7f42-4c1e-9d3a-5be1f00d0002",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(state(&paid), ("paid", None), "{paid}");
    app.assert_reconciled(&key).await;
    page.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn manual_capture_stops_at_the_hold_and_the_merchant_captures() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("shop").await;
    let page = Page::new(&app);
    let (id, secret, _) = session(
        &app,
        &key,
        json!({"amount": 700, "currency": "KWD", "capture_method": "manual"}),
    )
    .await;
    let (_, held) = page
        .pay(
            &id,
            &secret,
            "4b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(state(&held), ("authorized", None), "{held}");
    assert_eq!(held["session"]["amount_display"], "0.700");
    let (_, merchant) = app.get(&format!("/v1/checkout_sessions/{id}"), &key).await;
    let pid = merchant["attempts"][0]["payment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            &key,
            Some("merchant-capture"),
            json!({"amount": 700}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(state(&page.view(&id, &secret).await.1), ("paid", None));
    page.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn state_changes_come_only_from_the_checkout_origin_and_never_carry_card_data() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("shop").await;
    let page = Page::new(&app);
    let (id, secret, _) = session(&app, &key, json!({"amount": 100, "currency": "USD"})).await;
    let bearer = format!("Bearer {secret}");
    let path = format!("/api/sessions/{id}/attempts");
    let body = json!({"token": "tok_sim_approve"});
    let idem = ("idempotency-key", "5b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001");
    for headers in [
        vec![("authorization", bearer.as_str()), idem],
        vec![
            ("authorization", &bearer),
            ("origin", "http://127.0.0.1:56084"),
            idem,
        ],
        vec![("authorization", &bearer), ("origin", "null"), idem],
        vec![
            ("authorization", &bearer),
            ("origin", ORIGIN),
            ("sec-fetch-site", "same-site"),
            idem,
        ],
    ] {
        let (status, _, refused) = raw(
            &page.router,
            Method::POST,
            &path,
            &headers,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}");
        assert_eq!(refused["error"]["code"], "cross_origin_refused");
    }
    // A browser's cross-site read is refused too.
    let (status, _, _) = raw(
        &page.router,
        Method::GET,
        &format!("/api/sessions/{id}"),
        &[("authorization", &bearer), ("sec-fetch-site", "cross-site")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Card data is refused before anything happens, whatever field carries it.
    for card in [
        json!({"token": "4242 4242 4242 4242"}),
        json!({"token": "tok_sim_approve", "card": {"number": "x"}}),
    ] {
        let (status, _, refused) = raw(
            &page.router,
            Method::POST,
            &path,
            &[("authorization", &bearer), ("origin", ORIGIN), idem],
            Some(card),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    }
    let (status, _, refused) = raw(
        &page.router,
        Method::POST,
        &path,
        &[("authorization", &bearer), ("origin", ORIGIN), idem],
        Some(json!({"token": "tok_sim_free_money"})),
    )
    .await;
    assert_eq!(
        (status, refused["error"]["code"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("invalid_payment_token")
        )
    );
    assert_eq!(
        sim.store.count().await.unwrap(),
        0,
        "nothing reached the network"
    );
    let (_, view) = page.view(&id, &secret).await;
    assert_eq!(state(&view), ("open", Some("pay")));
    page.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one page's policy, every case side by side"
)]
async fn the_page_is_framed_only_by_its_sessions_origins_under_a_hashed_policy() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("shop").await;
    let page = Page::new(&app);
    let (embeddable, _, _) = session(
        &app,
        &key,
        json!({"amount": 100, "currency": "USD", "embed_origins": ["http://LOCALHOST:56470"]}),
    )
    .await;
    let (plain, _, _) = session(&app, &key, json!({"amount": 100, "currency": "USD"})).await;
    // sha256("start()"), computed here, independently of the server's code.
    let hash = format!(
        "'sha256-{}'",
        sandbox_server::web::base64(&sha2::Sha256::digest(b"start()"))
    );
    assert_eq!(
        hash,
        "'sha256-DIm7WJS6ZKDYe5qFLPy+h4JFI9Bol5QmYC57mt3Fb00='"
    );
    for (path, ancestors, framed) in [
        (
            format!("/c/{embeddable}"),
            "frame-ancestors http://localhost:56470",
            true,
        ),
        (format!("/c/{plain}"), "frame-ancestors 'none'", false),
        (
            "/c/not-a-session".to_owned(),
            "frame-ancestors 'none'",
            false,
        ),
        ("/".to_owned(), "frame-ancestors 'none'", false),
    ] {
        let (status, headers, _) = raw(&page.router, Method::GET, &path, &[], None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let csp = headers["content-security-policy"].to_str().unwrap();
        assert!(csp.contains(ancestors), "{path}: {csp}");
        assert!(csp.contains(&format!("script-src 'self' {hash}")), "{csp}");
        assert!(csp.contains("style-src 'self';"), "{csp}");
        assert!(
            !csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"),
            "{csp}"
        );
        assert!(csp.contains("default-src 'none'") && csp.contains("form-action 'none'"));
        assert_eq!(headers.contains_key("x-frame-options"), !framed, "{path}");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["referrer-policy"], "no-referrer");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["sandbox-simulated"], "true");
        assert!(
            headers["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
    }
    let (status, headers, _) = raw(
        &page.router,
        Method::GET,
        "/_app/immutable/start.js",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    assert_eq!(
        raw(
            &page.router,
            Method::GET,
            "/_app/immutable/missing.js",
            &[],
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, _, body) = raw(&page.router, Method::GET, "/api/nothing", &[], None).await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("route_not_found"))
    );
    // The checkout origin serves no /v1.
    let (status, _, _) = raw(
        &page.router,
        Method::GET,
        "/v1/payments",
        &[("authorization", &format!("Bearer {key}"))],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    page.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn session_fields_that_could_deceive_a_payer_or_escape_the_page_are_refused() {
    let app = TestApp::new(&[]).await;
    let (_, key) = app.project("shop").await;
    for (body, needle) in [
        (json!({"amount": 0, "currency": "USD"}), "amount"),
        (json!({"amount": 5, "currency": "XYZ"}), "currency"),
        (
            json!({"amount": 5, "currency": "USD", "description": "pay\u{202e}321"}),
            "description",
        ),
        (
            json!({"amount": 5, "currency": "USD", "embed_origins": ["*"]}),
            "embed_origins",
        ),
        (
            json!({"amount": 5, "currency": "USD", "embed_origins": ["https://example.com:443"]}),
            "embed_origins",
        ),
        (
            json!({"amount": 5, "currency": "USD", "return_url": "javascript:alert(1)"}),
            "return_url",
        ),
        (
            json!({"amount": 5, "currency": "USD", "return_url": "http://evil.example/"}),
            "return_url",
        ),
        (
            json!({"amount": 5, "currency": "USD", "expires_in": 5}),
            "expires_in",
        ),
        (
            json!({"amount": 5, "currency": "USD", "capture_method": "later"}),
            "capture_method",
        ),
    ] {
        let (status, refused) = app
            .post("/v1/checkout_sessions", &key, None, body.clone())
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {refused}"
        );
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains(needle),
            "{body}: {refused}"
        );
    }
    let (read_id, read_key) = app.key(&key, &["read"]).await;
    let _ = read_id;
    let (status, _) = app
        .post(
            "/v1/checkout_sessions",
            &read_key,
            None,
            json!({"amount": 5, "currency": "USD"}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "creating a session moves money"
    );
    app.cleanup().await;
}

#[tokio::test]
async fn a_session_allows_a_bounded_number_of_attempts_and_none_after_it_expires() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (project, key) = app.project("shop").await;
    let page = Page::new(&app);
    let (id, secret, _) = session(&app, &key, json!({"amount": 100, "currency": "USD"})).await;
    for n in 1..=10 {
        let (_, declined) = page
            .pay(
                &id,
                &secret,
                &format!("6b1e6c8a-7f42-4c1e-9d3a-5be1f00d{n:04x}"),
                "tok_sim_decline_expired",
            )
            .await;
        assert_eq!(declined["session"]["state"], "declined", "{declined}");
    }
    let (_, last) = page.view(&id, &secret).await;
    assert_eq!(
        state(&last),
        ("declined", None),
        "no more attempts: nothing to offer"
    );
    assert_eq!(last["session"]["attempts_left"], 0);
    let (status, refused) = page
        .pay(
            &id,
            &secret,
            "6b1e6c8a-7f42-4c1e-9d3a-5be1f00dffff",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(
        (status, refused["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("checkout_attempts_exhausted"))
    );

    // An expired session refuses a new attempt (its row moved into the past by the owner).
    let (expiring, secret, _) =
        session(&app, &key, json!({"amount": 100, "currency": "USD"})).await;
    let owner = sqlx::PgPool::connect(&app.db.owner_url()).await.unwrap();
    sqlx::query(
        "UPDATE checkout_sessions SET created_at = now() - interval '2 hours',
                expires_at = now() - interval '1 hour'
          WHERE project_id = $1::uuid AND id = $2::uuid",
    )
    .bind(&project)
    .bind(&expiring)
    .execute(&owner)
    .await
    .unwrap();
    owner.close().await;
    let (_, view) = page.view(&expiring, &secret).await;
    assert_eq!(
        (state(&view), &view["session"]["expired"]),
        (("open", None), &json!(true))
    );
    let (status, refused) = page
        .pay(
            &expiring,
            &secret,
            "7b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(
        (status, refused["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("checkout_expired"))
    );
    page.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn payments_are_listed_newest_first_in_pages() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("shop").await;
    for n in 0..7 {
        let (status, _) = app
            .authorize(
                &key,
                &format!("page-{n}"),
                100 + n,
                "USD",
                "tok_sim_approve",
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let mut seen = Vec::new();
    let mut path = "/v1/payments?limit=3".to_owned();
    loop {
        let (status, page) = app.get(&path, &key).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        seen.extend(
            page["payments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["amount"].as_i64().unwrap()),
        );
        match page["next_before"].as_str() {
            Some(before) => path = format!("/v1/payments?limit=3&before={before}"),
            None => break,
        }
    }
    assert_eq!(seen, vec![106, 105, 104, 103, 102, 101, 100]);
    let (_, unknown) = app
        .get(
            "/v1/payments?before=0b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001",
            &key,
        )
        .await;
    assert_eq!(unknown["payments"], json!([]));
    app.cleanup().await;
    sim.remove().await;
}

/// Restores `source`'s database, as it is now, into the second cluster (D-055) with the
/// restore rehearsal (`pg_dump`, `pg_restore`), and returns the restored database.
async fn backup_into_the_restore_cluster(source: &TestApp) -> sandbox_db::testing::TestDb {
    let other = std::env::var("PS_TEST_RESTORE_DATABASE_URL").expect(
        "PS_TEST_RESTORE_DATABASE_URL names the second cluster's administrator (scripts/stack.sh env)",
    );
    let container = std::env::var("PS_PG_RESTORE_CONTAINER")
        .unwrap_or_else(|_| "payment-sandbox-postgres-restore-1".to_owned());
    let mut admin = <sqlx::PgConnection as sqlx::Connection>::connect(&other)
        .await
        .unwrap();
    sandbox_db::roles::bootstrap(&mut admin).await.unwrap();
    let target = format!("ps_test_{}", uuid::Uuid::now_v7().simple());
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/restore-rehearsal.sh"
    );
    let rehearsal = std::process::Command::new(script)
        .arg(source.db.name())
        .arg(&target)
        .env("PS_PG_TARGET_CONTAINER", &container)
        .output()
        .unwrap();
    assert!(
        rehearsal.status.success(),
        "the rehearsal failed: {}{}",
        String::from_utf8_lossy(&rehearsal.stdout),
        String::from_utf8_lossy(&rehearsal.stderr)
    );
    sandbox_db::testing::TestDb::existing(&other, &target).await
}

/// Review M1: after a restore the server reuses an attempt number the network already
/// answered. A different scenario under that number is a network conflict; it must close
/// the session, never count as a decline that admits a second, charging attempt.
#[tokio::test]
async fn after_a_restore_a_conflicting_attempt_closes_the_session_and_never_charges_twice() {
    let sim = TestSim::start().await;
    let vars = common::sim_vars(&sim, &[]);
    let source = TestApp::on(sandbox_db::testing::TestDb::new().await, &vars);
    let (_, key) = source.project("shop").await;
    let (id, secret, _) = session(&source, &key, json!({"amount": 2_500, "currency": "USD"})).await;
    // The backup is older than the payment.
    let backup = backup_into_the_restore_cluster(&source).await;
    let page = Page::new(&source);
    let (_, paid) = page
        .pay(
            &id,
            &secret,
            "c1e6c8a0-7f42-4c1e-9d3a-5be1f00d0001",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(state(&paid), ("paid", None), "{paid}");
    assert_eq!(sim.store.count().await.unwrap(), 2);
    page.remove();
    source.cleanup().await;

    // The server comes back from the older backup: it has the session, not the attempt.
    let restored = TestApp::on(backup, &vars);
    let page = Page::new(&restored);
    let (_, open) = page.view(&id, &secret).await;
    assert_eq!(state(&open), ("open", Some("pay")), "{open}");
    // A declining scenario reuses attempt 1's identity; the network holds the approval.
    let (status, conflict) = page
        .pay(
            &id,
            &secret,
            "c1e6c8a0-7f42-4c1e-9d3a-5be1f00d0002",
            "tok_sim_decline_do_not_honor",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{conflict}");
    assert_eq!(state(&conflict), ("conflict", None), "{conflict}");
    // Nothing can start another attempt, so nothing can charge again.
    let (status, refused) = page
        .pay(
            &id,
            &secret,
            "c1e6c8a0-7f42-4c1e-9d3a-5be1f00d0003",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(
        (status, refused["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("checkout_closed")),
        "{refused}"
    );
    assert_eq!(
        sim.store.count().await.unwrap(),
        2,
        "one authorization and one capture at the network, never a second charge"
    );
    // And the disagreement is reported, not hidden.
    let (_, report) = restored.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(report["clean"], false, "{report}");
    page.remove();
    restored.cleanup().await;
    sim.remove().await;
}

/// The reason attempt keys keep their numbers across a restore (D-015, D-065): the same
/// payer paying again after a restore reaches the network's record of the payment, which
/// is recovered once, instead of becoming a second, new charge.
#[tokio::test]
async fn after_a_restore_paying_again_recovers_the_payment_without_a_second_charge() {
    let sim = TestSim::start().await;
    let vars = common::sim_vars(&sim, &[]);
    let first = TestApp::on(sandbox_db::testing::TestDb::new().await, &vars);
    let (_, key) = first.project("shop").await;
    let (id, secret, _) = session(&first, &key, json!({"amount": 2_500, "currency": "USD"})).await;
    first.state.db.close().await;
    let TestApp { db: mut live, .. } = first;
    let backup = live.snapshot().await;
    let after = TestApp::on(live, &vars);
    let page = Page::new(&after);
    let (_, paid) = page
        .pay(
            &id,
            &secret,
            "d1e6c8a0-7f42-4c1e-9d3a-5be1f00d0001",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(state(&paid), ("paid", None), "{paid}");
    page.remove();
    after.cleanup().await;

    let restored = TestApp::on(backup, &vars);
    let page = Page::new(&restored);
    // A fresh tab (a new key), the same scenario: attempt 1 again, answered from the
    // network's record.
    let (_, recovered) = page
        .pay(
            &id,
            &secret,
            "d1e6c8a0-7f42-4c1e-9d3a-5be1f00d0002",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(state(&recovered), ("paid", None), "{recovered}");
    assert_eq!(sim.store.count().await.unwrap(), 2, "no second charge");
    let (_, report) = restored.get("/v1/reconciliation?network=true", &key).await;
    assert_eq!(report["clean"], true, "{report}");
    page.remove();
    restored.cleanup().await;
    sim.remove().await;
}
