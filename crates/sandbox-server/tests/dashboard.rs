//! The dashboard's origin (D-061, D-062, D-063): a sandbox key exchanged for a host-only
//! session; every state change needs this origin and the session's CSRF token; the reads
//! are the API's own, read-only; a revoked key ends its sessions at once; the simulator's
//! controls exist only in the dev profile, with the operator's own credential.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderMap, Method, StatusCode};
use common::{SIM_CONTROL_TOKEN, TestApp, TestSim, raw, test_site};
use sandbox_server::web::dashboard::Sessions;
use serde_json::{Value, json};

const ORIGIN: &str = "http://dashboard.localhost:56084";
/// The dashboard's own host name (D-065): its cookie is never sent to another port's site.
const HOST: &str = "dashboard.localhost:56084";

struct Dash {
    router: Router,
    dir: std::path::PathBuf,
    sessions: Arc<Sessions>,
}

impl Dash {
    fn new(app: &TestApp) -> Self {
        let (dir, site) = test_site();
        let sessions = Arc::new(Sessions::default());
        Self {
            router: sandbox_server::web::dashboard::router(
                &app.state,
                site,
                ORIGIN,
                sessions.clone(),
            ),
            dir,
            sessions,
        }
    }

    /// Signs in with `key`; returns the cookie pair and the CSRF token.
    async fn sign_in(&self, key: &str) -> (String, String) {
        let bearer = format!("Bearer {key}");
        let (status, headers, body) = self
            .raw(
                Method::POST,
                "/api/session",
                &[
                    ("authorization", &bearer),
                    ("origin", ORIGIN),
                    ("sec-fetch-site", "same-origin"),
                ],
                None,
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let set = headers["set-cookie"].to_str().unwrap().to_owned();
        let pair = set.split(';').next().unwrap().to_owned();
        (pair, body["csrf_token"].as_str().unwrap().to_owned())
    }

    /// One request on the dashboard's own host name.
    async fn raw(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut all = vec![("host", HOST)];
        all.extend_from_slice(headers);
        raw(&self.router, method, path, &all, body).await
    }

    /// A read as the dashboard's page makes it (a browser sends fetch metadata).
    async fn get(&self, path: &str, cookie: &str) -> (StatusCode, HeaderMap, Value) {
        self.raw(
            Method::GET,
            path,
            &[("cookie", cookie), ("sec-fetch-site", "same-origin")],
            None,
        )
        .await
    }

    async fn post(
        &self,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Value) {
        self.raw(Method::POST, path, headers, body).await
    }

    fn remove(self) {
        let _ = std::fs::remove_dir_all(self.dir);
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the whole session lifecycle, in order"
)]
async fn sign_in_gives_a_host_only_session_and_every_state_change_needs_origin_and_csrf() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, admin) = app.project("shop").await;
    let (_, read) = app.key(&admin, &["read"]).await;
    app.authorize(&admin, "dash-1", 1_000, "USD", "tok_sim_approve")
        .await;
    let dash = Dash::new(&app);

    // Sign-in needs this origin too: no login CSRF.
    let bearer = format!("Bearer {read}");
    for headers in [
        vec![("authorization", bearer.as_str())],
        vec![
            ("authorization", &bearer),
            ("origin", "http://127.0.0.1:56083"),
        ],
    ] {
        let (status, _, body) = dash.post("/api/session", &headers, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    let (status, _, _) = dash
        .post(
            "/api/session",
            &[("authorization", "Bearer sk_sim_nope"), ("origin", ORIGIN)],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let bearer = format!("Bearer {read}");
    let (status, headers, body) = dash
        .post(
            "/api/session",
            &[("authorization", &bearer), ("origin", ORIGIN)],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let set = headers["set-cookie"].to_str().unwrap();
    assert!(set.starts_with("__Host-ps_dashboard="), "{set}");
    assert!(
        set.ends_with("; Path=/; Secure; HttpOnly; SameSite=Strict"),
        "{set}"
    );
    assert!(!set.contains("Domain"), "{set}");
    assert_eq!(body["project"]["slug"], "shop");
    assert_eq!(body["scopes"], json!(["read"]));
    assert_eq!(
        body["sim_controls"], true,
        "the test app runs in the dev profile"
    );
    let cookie = set.split(';').next().unwrap().to_owned();
    let csrf = body["csrf_token"].as_str().unwrap().to_owned();
    assert!(!csrf.bytes().any(|b| b.is_ascii_digit()));

    // The API's own reads, through the session.
    let (status, _, payments) = dash.get("/api/v1/payments", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{payments}");
    assert_eq!(payments["payments"].as_array().unwrap().len(), 1);
    let (status, _, current) = dash.get("/api/session/current", &cookie).await;
    assert_eq!(
        (status, &current["csrf_token"]),
        (StatusCode::OK, &json!(csrf))
    );
    // No session, no reads; a key is no credential on this origin; a doubled cookie is
    // trusted by neither copy; a browser's cross-site read is refused.
    assert_eq!(
        dash.raw(
            Method::GET,
            "/api/v1/payments",
            &[("sec-fetch-site", "same-origin")],
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let key_bearer = format!("Bearer {admin}");
    assert_eq!(
        dash.raw(
            Method::GET,
            "/api/v1/payments",
            &[
                ("authorization", &key_bearer),
                ("sec-fetch-site", "same-origin")
            ],
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let doubled = format!("{cookie}; __Host-ps_dashboard=planted");
    assert_eq!(
        dash.get("/api/v1/payments", &doubled).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, _) = dash
        .raw(
            Method::GET,
            "/api/v1/payments",
            &[("cookie", &cookie), ("sec-fetch-site", "same-site")],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Nothing that moves money is mounted here.
    for path in ["/api/v1/payments", "/api/v1/checkout_sessions"] {
        let (status, _, _) = dash
            .post(
                path,
                &[
                    ("cookie", &cookie),
                    ("origin", ORIGIN),
                    ("x-csrf-token", &csrf),
                ],
                Some(json!({"amount": 5})),
            )
            .await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
            "{path}: {status}"
        );
    }

    // Sign-out is a state change: this origin and the CSRF token.
    for headers in [
        vec![("cookie", cookie.as_str()), ("origin", ORIGIN)],
        vec![
            ("cookie", &cookie),
            ("origin", ORIGIN),
            ("x-csrf-token", "wrong"),
        ],
        vec![("cookie", &cookie), ("x-csrf-token", &csrf)],
    ] {
        let (status, _, _) = dash.post("/api/session/sign-out", &headers, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}");
    }
    let (status, headers, _) = dash
        .post(
            "/api/session/sign-out",
            &[
                ("cookie", &cookie),
                ("origin", ORIGIN),
                ("x-csrf-token", &csrf),
            ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        dash.get("/api/v1/payments", &cookie).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(dash.sessions.is_empty());
    dash.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn a_revoked_key_ends_its_dashboard_sessions_at_once_and_sign_in_rotates_the_session() {
    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("shop").await;
    let (read_id, read) = app.key(&admin, &["read"]).await;
    let dash = Dash::new(&app);
    let (cookie, _) = dash.sign_in(&read).await;
    assert_eq!(
        dash.get("/api/v1/balances", &cookie).await.0,
        StatusCode::OK
    );
    // A second sign-in from the same browser ends the first session (no fixation).
    let bearer = format!("Bearer {read}");
    let (status, headers, _) = dash
        .post(
            "/api/session",
            &[
                ("authorization", &bearer),
                ("origin", ORIGIN),
                ("cookie", &cookie),
            ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let rotated = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert_ne!(rotated, cookie);
    assert_eq!(
        dash.get("/api/v1/balances", &cookie).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        dash.get("/api/v1/balances", &rotated).await.0,
        StatusCode::OK
    );

    let (status, _) = app
        .post(
            &format!("/v1/keys/{read_id}/revoke"),
            &admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        dash.get("/api/v1/balances", &rotated).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(dash.sessions.is_empty(), "the session ended with its key");
    dash.remove();
    app.cleanup().await;
}

#[tokio::test]
async fn explanations_page_through_the_dashboard_with_sealed_cursors() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, admin) = app.project("shop").await;
    for n in 0..4 {
        app.authorize(
            &admin,
            &format!("explain-{n}"),
            250,
            "USD",
            "tok_sim_approve",
        )
        .await;
    }
    let dash = Dash::new(&app);
    let (cookie, _) = dash.sign_in(&admin).await;
    let mut path = "/api/v1/balances/USD/payer_funding/explain?limit=3".to_owned();
    let mut lines = 0;
    let mut sum = 0_i64;
    let balance = loop {
        let (status, _, page) = dash.get(&path, &cookie).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        for line in page["explanation"]["lines"].as_array().unwrap() {
            lines += 1;
            sum += line["amount_minor"].as_i64().unwrap();
        }
        match page["explanation"]["next"]["cursor"].as_str() {
            Some(cursor) => {
                path =
                    format!("/api/v1/balances/USD/payer_funding/explain?limit=3&cursor={cursor}");
            }
            None => break page["explanation"]["balance_minor"].as_i64().unwrap(),
        }
    };
    assert_eq!(lines, 4);
    assert_eq!(sum, balance, "the lines sum to the balance");
    let (status, _, report) = dash.get("/api/v1/reconciliation", &cookie).await;
    assert_eq!(
        (status, &report["clean"]),
        (StatusCode::OK, &json!(true)),
        "{report}"
    );
    let (status, _, tasks) = dash.get("/api/status", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{tasks}");
    dash.remove();
    app.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn simulator_controls_need_the_dev_profile_and_the_operators_own_credential() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, admin) = app.project("shop").await;
    let dash = Dash::new(&app);
    let (cookie, csrf) = dash.sign_in(&admin).await;
    let rule = json!({"kind": "authorize", "action": "unavailable", "count": 1});
    let base = [
        ("cookie", cookie.as_str()),
        ("origin", ORIGIN),
        ("x-csrf-token", csrf.as_str()),
    ];
    let (status, _, refused) = dash
        .post("/api/sim/faults", &base, Some(rule.clone()))
        .await;
    assert_eq!(
        (status, refused["error"]["code"].as_str()),
        (
            StatusCode::UNAUTHORIZED,
            Some("operator_credential_required")
        ),
        "{refused}"
    );
    let mut wrong = base.to_vec();
    wrong.push((
        "x-operator-token",
        "not-the-operators-credential-0123456789",
    ));
    assert_eq!(
        dash.post("/api/sim/faults", &wrong, Some(rule.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let mut right = base.to_vec();
    right.push(("x-operator-token", SIM_CONTROL_TOKEN));
    let (status, _, added) = dash
        .post("/api/sim/faults", &right, Some(rule.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    // The rule reached the network: the next authorization is unavailable, so uncertain.
    let (status, reply) = app
        .authorize(&admin, "after-fault", 100, "USD", "tok_sim_approve")
        .await;
    assert_eq!(
        (status, &reply["operation"]["state"]),
        (StatusCode::ACCEPTED, &json!("uncertain")),
        "{reply}"
    );
    // Without the CSRF token the credential alone is not enough.
    let no_csrf = [
        ("cookie", cookie.as_str()),
        ("origin", ORIGIN),
        ("x-operator-token", SIM_CONTROL_TOKEN),
    ];
    assert_eq!(
        dash.post("/api/sim/faults/clear", &no_csrf, None).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, _, clock) = dash
        .raw(
            Method::GET,
            "/api/sim/clock",
            &[
                ("cookie", &cookie),
                ("x-operator-token", SIM_CONTROL_TOKEN),
                ("sec-fetch-site", "same-origin"),
            ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{clock}");
    assert!(clock["network"]["now_seconds"].is_i64());
    dash.remove();
    app.cleanup().await;

    // Outside the dev profile the routes do not exist.
    let ci = TestApp::with_sim(&sim, &[("PS_PROFILE", "ci"), ("PS_WEBHOOK_PORTS", "9")]).await;
    let (_, admin) = ci.project("shop").await;
    let dash = Dash::new(&ci);
    let (cookie, csrf) = dash.sign_in(&admin).await;
    let (status, _, _) = dash
        .post(
            "/api/sim/faults",
            &[
                ("cookie", &cookie),
                ("origin", ORIGIN),
                ("x-csrf-token", &csrf),
                ("x-operator-token", SIM_CONTROL_TOKEN),
            ],
            Some(rule),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, me) = dash.get("/api/session/current", &cookie).await;
    assert_eq!(
        (&me["sim_controls"], &me["profile"]),
        (&json!(false), &json!("ci"))
    );
    dash.remove();
    ci.cleanup().await;
    sim.remove().await;
}

#[tokio::test]
async fn the_dashboard_is_never_framed_and_its_scripts_are_pinned_by_hash() {
    let app = TestApp::new(&[]).await;
    let dash = Dash::new(&app);
    for path in ["/", "/payments", "/ledger/USD/payer_funding"] {
        let (status, headers, _) = dash.raw(Method::GET, path, &[], None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let csp = headers["content-security-policy"].to_str().unwrap();
        assert!(
            csp.contains("frame-ancestors 'none'") && csp.contains("'sha256-"),
            "{csp}"
        );
        assert!(!csp.contains("unsafe-inline"), "{csp}");
        assert_eq!(headers["x-frame-options"], "DENY");
        assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
    }
    let (status, headers, _) = dash
        .raw(
            Method::GET,
            "/api/nothing",
            &[("sec-fetch-site", "same-origin")],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers["x-frame-options"], "DENY");
    dash.remove();
    app.cleanup().await;
}

/// Review M2: browsers send a cookie to every port of its host, and a request that carries
/// the cookie but no fetch metadata (an older client, a non-browser) used to be answered.
/// Now every API request needs fetch metadata saying same-origin, or this exact Origin; a
/// non-browser client uses `/v1` with its key instead.
#[tokio::test]
async fn reads_need_fetch_metadata_or_this_origin_even_with_the_cookie() {
    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("shop").await;
    let dash = Dash::new(&app);
    let (cookie, _) = dash.sign_in(&admin).await;
    let bare = dash
        .raw(
            Method::GET,
            "/api/v1/balances",
            &[("cookie", &cookie)],
            None,
        )
        .await;
    assert_eq!(bare.0, StatusCode::FORBIDDEN, "{}", bare.2);
    assert_eq!(bare.2["error"]["code"], "cross_origin_refused");
    let other = dash
        .raw(
            Method::GET,
            "/api/v1/balances",
            &[("cookie", &cookie), ("origin", "http://127.0.0.1:56083")],
            None,
        )
        .await;
    assert_eq!(other.0, StatusCode::FORBIDDEN);
    let with_origin = dash
        .raw(
            Method::GET,
            "/api/v1/balances",
            &[("cookie", &cookie), ("origin", ORIGIN)],
            None,
        )
        .await;
    assert_eq!(with_origin.0, StatusCode::OK, "{}", with_origin.2);
    assert_eq!(
        dash.get("/api/v1/balances", &cookie).await.0,
        StatusCode::OK
    );
    dash.remove();
    app.cleanup().await;
}

/// Review M2: the dashboard answers on its own host name only, so its cookie is never set
/// for (and so never sent to) the host the checkout and every other loopback port share.
#[tokio::test]
async fn the_dashboard_answers_only_on_its_own_host_name() {
    let app = TestApp::new(&[]).await;
    let dash = Dash::new(&app);
    for host in [
        "127.0.0.1:56084",
        "localhost:56084",
        "dashboard.localhost:56085",
    ] {
        let (status, headers, _) = raw(
            &dash.router,
            Method::GET,
            "/payments?x=1",
            &[("host", host)],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::PERMANENT_REDIRECT, "{host}");
        assert_eq!(
            headers["location"],
            format!("{ORIGIN}/payments?x=1"),
            "{host}"
        );
        let (status, _, refused) = raw(
            &dash.router,
            Method::POST,
            "/api/session",
            &[
                ("host", host),
                ("origin", ORIGIN),
                ("authorization", "Bearer sk_sim_x"),
            ],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{host}: {refused}");
    }
    let (status, _, _) = dash.raw(Method::GET, "/payments", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    dash.remove();
    app.cleanup().await;
}
