//! `docs/API.md` and the server's routes are the same list (v1 review M3, D-084): every
//! route `routes.rs` serves has a row in the reference's endpoint table, every row is served
//! (a request to it never answers `route_not_found` or `405`), and the reference lists every
//! stable error code. SIMULATED MONEY ONLY.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::collections::BTreeSet;

use axum::http::{Method, StatusCode};
use common::{BOOTSTRAP, TestApp};
use serde_json::json;

const ROUTES: &str = include_str!("../src/routes.rs");
const ERRORS: &str = include_str!("../src/error.rs");
const REFERENCE: &str = include_str!("../../../docs/API.md");

/// `(METHOD, /path)` for every `.route("/path", get(..).post(..))` in `routes.rs`.
fn served() -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for chunk in ROUTES.split(".route(").skip(1) {
        let Some(rest) = chunk.trim_start().strip_prefix('"') else {
            continue;
        };
        let (path, handlers) = rest.split_once('"').unwrap();
        // The handlers end where the next builder call starts.
        let handlers = handlers
            .split([';'])
            .next()
            .unwrap()
            .split(".route_layer(")
            .next()
            .unwrap();
        for method in ["get", "post"] {
            let called = handlers
                .match_indices(&format!("{method}("))
                .any(|(at, _)| {
                    at == 0
                        || !handlers.as_bytes()[at - 1].is_ascii_alphanumeric()
                            && !matches!(handlers.as_bytes()[at - 1], b'_' | b':')
                });
            if called {
                out.insert((method.to_ascii_uppercase(), path.to_owned()));
            }
        }
    }
    out
}

/// `(METHOD, /path)` for every row of the reference's endpoint table.
fn documented() -> BTreeSet<(String, String)> {
    REFERENCE
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|row| row.split_once('`'))
        .filter_map(|(route, _)| route.split_once(' '))
        .filter(|(method, _)| matches!(*method, "GET" | "POST"))
        .map(|(method, path)| (method.to_owned(), path.to_owned()))
        .collect()
}

#[test]
fn every_route_is_in_the_reference_and_every_row_is_a_route() {
    let served = served();
    assert!(served.len() >= 25, "the route parser found {served:?}");
    let documented = documented();
    let missing: Vec<_> = served.difference(&documented).collect();
    let extra: Vec<_> = documented.difference(&served).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "served but not in docs/API.md: {missing:?}; in docs/API.md but not served: {extra:?}"
    );
}

#[test]
fn the_reference_lists_every_error_code() {
    let enum_body = ERRORS
        .split("pub enum ErrorCode {")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    let codes: Vec<String> = enum_body
        .lines()
        .map(str::trim)
        .filter(|l| l.ends_with(',') && l.chars().next().is_some_and(char::is_uppercase))
        .map(|variant| {
            let mut code = String::new();
            for (i, c) in variant.trim_end_matches(',').chars().enumerate() {
                if c.is_uppercase() && i > 0 {
                    code.push('_');
                }
                code.push(c.to_ascii_lowercase());
            }
            code
        })
        .collect();
    assert!(codes.len() > 30, "{codes:?}");
    for code in codes {
        assert!(
            REFERENCE.contains(&format!("`{code}`")),
            "docs/API.md does not list the error code {code}"
        );
    }
}

#[tokio::test]
async fn every_documented_route_is_served() {
    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("reference").await;
    let some_id = "019a0000-0000-7000-8000-000000000000";
    for (method, path) in documented() {
        let concrete = path
            .replace("{payment_id}", some_id)
            .replace("{key_id}", some_id)
            .replace("{endpoint_id}", some_id)
            .replace("{id}", some_id)
            .replace("{currency}", "USD")
            .replace("{account}", "payer_funding");
        let bearer = if path == "/v1/projects" {
            BOOTSTRAP
        } else {
            &admin
        };
        let (status, body) = if method == "POST" {
            // An empty body: refused for its content, never for its route.
            app.send(
                Method::POST,
                &concrete,
                Some(bearer),
                Some("reference-check"),
                Some(json!({"probe": true})),
            )
            .await
        } else {
            app.send(Method::GET, &concrete, Some(bearer), None, None)
                .await
        };
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}: {body}"
        );
        assert_ne!(
            body["error"]["code"], "route_not_found",
            "{method} {path}: {body}"
        );
    }
    app.cleanup().await;
}
