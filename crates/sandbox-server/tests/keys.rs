//! M1: projects and sandbox keys. Bootstrap, scopes, revocation, isolation between
//! projects, and secrets kept out of the logs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::{BOOTSTRAP, TestApp};
use serde_json::json;

#[tokio::test]
async fn creating_a_project_needs_the_operator_bootstrap_token() {
    let app = TestApp::new(&[]).await;
    let body = json!({"slug": "store", "name": "Store"});
    for bearer in [None, Some("wrong-token-wrong-token-wrong-token-x")] {
        let (status, reply) = app
            .send(
                Method::POST,
                "/v1/projects",
                bearer,
                None,
                Some(body.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{reply}");
        // Its own refusal, naming the credential it needs and where it comes from (v1
        // review M5), not "missing or invalid sandbox key".
        assert_eq!(
            reply["error"]["code"], "bootstrap_token_required",
            "{reply}"
        );
        let message = reply["error"]["message"].as_str().unwrap();
        assert!(message.contains("PS_BOOTSTRAP_TOKEN"), "{message}");
        assert!(!message.contains("sandbox key"), "{message}");
    }
    let (status, reply) = app
        .send(
            Method::POST,
            "/v1/projects",
            Some(BOOTSTRAP),
            None,
            Some(body),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{reply}");
    let secret = reply["api_key"]["secret"].as_str().unwrap();
    assert!(
        secret.starts_with("sk_sim_") && secret.len() == 71,
        "{secret}"
    );
    assert_eq!(reply["api_key"]["scopes"], json!(["admin"]));
    assert_eq!(reply["simulated"], true);
    // An admin key is not a bootstrap token.
    let (status, _) = app
        .send(
            Method::POST,
            "/v1/projects",
            Some(secret),
            None,
            Some(json!({"slug": "other", "name": "x"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    app.cleanup().await;
}

#[tokio::test]
async fn without_a_configured_bootstrap_token_nobody_can_create_projects() {
    let app = TestApp::new(&[]).await;
    let mut state = app.state.clone();
    let mut config = (*state.config).clone();
    config.bootstrap_token = None;
    state.config = Arc::new(config);
    let router = sandbox_server::app(&state);
    let request = axum::http::Request::post("/v1/projects")
        .header("authorization", format!("Bearer {BOOTSTRAP}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"slug":"s","name":"n"}"#))
        .unwrap();
    let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let reply: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        reply["error"]["code"], "bootstrap_token_required",
        "{reply}"
    );
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("started without PS_BOOTSTRAP_TOKEN"),
        "{message}"
    );
    app.cleanup().await;
}

#[tokio::test]
async fn slugs_are_unique_and_validated() {
    let app = TestApp::new(&[]).await;
    app.project("store").await;
    let (status, reply) = app
        .send(
            Method::POST,
            "/v1/projects",
            Some(BOOTSTRAP),
            None,
            Some(json!({"slug": "store", "name": "Again"})),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("conflict"))
    );
    for slug in ["Store", "-x", "a b", ""] {
        let (status, reply) = app
            .send(
                Method::POST,
                "/v1/projects",
                Some(BOOTSTRAP),
                None,
                Some(json!({"slug": slug, "name": "x"})),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{slug:?}: {reply}"
        );
    }
    app.cleanup().await;
}

#[tokio::test]
async fn scopes_decide_what_a_key_may_do() {
    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("store").await;
    let (_, reader) = app.key(&admin, &["read"]).await;
    let (_, writer) = app.key(&admin, &["payments:write"]).await;

    assert_eq!(app.get("/v1/project", &reader).await.0, StatusCode::OK);
    assert_eq!(
        app.get("/v1/project", &writer).await.0,
        StatusCode::OK,
        "payments:write implies read"
    );
    for key in [&reader, &writer] {
        let (status, reply) = app.get("/v1/keys", key).await;
        assert_eq!(
            (status, reply["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("forbidden"))
        );
        let (status, _) = app
            .post(
                "/v1/keys",
                key,
                None,
                json!({"label": "x", "scopes": ["admin"]}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, reply) = app.get("/v1/keys", &admin).await;
    assert_eq!(status, StatusCode::OK);
    let listed = reply["api_keys"].as_array().unwrap();
    assert_eq!(listed.len(), 3);
    assert!(
        !reply.to_string().contains(&reader),
        "a listing never carries secrets"
    );

    let (status, _) = app
        .post(
            "/v1/keys",
            &admin,
            None,
            json!({"label": "x", "scopes": ["owner"]}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = app
        .post(
            "/v1/keys",
            &admin,
            None,
            json!({"label": "x", "scopes": []}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    app.cleanup().await;
}

#[tokio::test]
async fn malformed_and_live_looking_keys_are_refused() {
    let app = TestApp::new(&[]).await;
    app.project("store").await;
    let hex = "0".repeat(64);
    for key in [
        format!("sk_live_{hex}"),
        format!("sk_test_{hex}"),
        format!("sk_sim_{hex}"),
        "garbage".to_owned(),
    ] {
        let (status, reply) = app.get("/v1/project", &key).await;
        assert_eq!(
            (status, reply["error"]["code"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized")),
            "{key}"
        );
    }
    let (status, _) = app.send(Method::GET, "/v1/project", None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    app.cleanup().await;
}

#[tokio::test]
async fn a_revoked_key_fails_on_its_very_next_request() {
    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("store").await;
    let (reader_id, reader) = app.key(&admin, &["read"]).await;
    assert_eq!(app.get("/v1/project", &reader).await.0, StatusCode::OK);
    let (status, _) = app
        .post(
            &format!("/v1/keys/{reader_id}/revoke"),
            &admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        app.get("/v1/project", &reader).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Revocation is idempotent.
    let (status, _) = app
        .post(
            &format!("/v1/keys/{reader_id}/revoke"),
            &admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    app.cleanup().await;
}

#[tokio::test]
async fn the_last_admin_key_cannot_be_revoked_even_by_two_concurrent_requests() {
    let app = TestApp::new(&[]).await;
    let (_, first) = app.project("store").await;
    let (_, listed) = app.get("/v1/keys", &first).await;
    let first_id = listed["api_keys"][0]["id"].as_str().unwrap().to_owned();
    let (status, reply) = app
        .post(
            &format!("/v1/keys/{first_id}/revoke"),
            &first,
            None,
            json!({}),
        )
        .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("invalid_state"))
    );

    // Two admins revoke each other at the same moment: exactly one may succeed.
    let (second_id, second) = app.key(&first, &["admin"]).await;
    let (path_a, path_b) = (
        format!("/v1/keys/{second_id}/revoke"),
        format!("/v1/keys/{first_id}/revoke"),
    );
    let a = app.post(&path_a, &first, None, json!({}));
    let b = app.post(&path_b, &second, None, json!({}));
    let ((sa, _), (sb, _)) = tokio::join!(a, b);
    let successes = [sa, sb].iter().filter(|s| **s == StatusCode::OK).count();
    assert_eq!(successes, 1, "{sa} {sb}");
    let mut alive = 0;
    for key in [&first, &second] {
        if app.get("/v1/keys", key).await.0 == StatusCode::OK {
            alive += 1;
        }
    }
    assert_eq!(alive, 1, "exactly one admin key survives");
    app.cleanup().await;
}

#[tokio::test]
async fn projects_cannot_see_or_touch_each_others_keys() {
    let app = TestApp::new(&[]).await;
    let (a_id, a_admin) = app.project("store-a").await;
    let (b_id, b_admin) = app.project("store-b").await;
    let (b_reader_id, b_reader) = app.key(&b_admin, &["read"]).await;

    let (_, a_keys) = app.get("/v1/keys", &a_admin).await;
    assert_eq!(a_keys["api_keys"].as_array().unwrap().len(), 1);
    assert!(!a_keys.to_string().contains(&b_reader_id));

    let (status, _) = app
        .post(
            &format!("/v1/keys/{b_reader_id}/revoke"),
            &a_admin,
            None,
            json!({}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another project's key is invisible"
    );
    let (status, reply) = app.get("/v1/project", &b_reader).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["project"]["id"], b_id.as_str());
    let (_, reply) = app.get("/v1/project", &a_admin).await;
    assert_eq!(reply["project"]["id"], a_id.as_str());
    app.cleanup().await;
}
