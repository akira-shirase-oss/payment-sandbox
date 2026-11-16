//! Shared helpers for the server's integration tests: an in-process app on a fresh
//! database, JSON requests, and project bootstrap.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: not every test uses every helper, and a panic is the failure report"
)]

use std::collections::HashMap;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use sandbox_db::testing::TestDb;
use sandbox_server::{AppState, Config, app};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The operator token the test apps are configured with.
pub const BOOTSTRAP: &str = "test-bootstrap-token-0123456789abcdef";

/// An app on its own database.
pub struct TestApp {
    pub db: TestDb,
    pub router: Router,
    pub state: AppState,
}

impl TestApp {
    pub async fn new(extra: &[(&str, &str)]) -> Self {
        let db = TestDb::new().await;
        let mut vars: HashMap<String, String> = HashMap::from([
            ("PS_DATABASE_URL".to_owned(), db.url()),
            ("PS_BOOTSTRAP_TOKEN".to_owned(), BOOTSTRAP.to_owned()),
        ]);
        for (k, v) in extra {
            vars.insert((*k).to_owned(), (*v).to_owned());
        }
        let config = Config::from_lookup(|name| vars.get(name).cloned()).unwrap();
        let pool = sandbox_db::connect_lazy(config.database_url.expose(), &config.pool).unwrap();
        let state = AppState::new(config, pool);
        let router = app(&state);
        Self { db, router, state }
    }

    /// Sends a request; `bearer` becomes `Authorization: Bearer …`.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        idempotency_key: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        if let Some(key) = idempotency_key {
            builder = builder.header("idempotency-key", key);
        }
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    pub async fn get(&self, path: &str, key: &str) -> (StatusCode, Value) {
        self.send(Method::GET, path, Some(key), None, None).await
    }

    pub async fn post(
        &self,
        path: &str,
        key: &str,
        idempotency_key: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        self.send(Method::POST, path, Some(key), idempotency_key, Some(body))
            .await
    }

    /// Creates a project; returns (project id, its admin key secret).
    pub async fn project(&self, slug: &str) -> (String, String) {
        let (status, body) = self
            .send(
                Method::POST,
                "/v1/projects",
                Some(BOOTSTRAP),
                None,
                Some(json!({"slug": slug, "name": slug})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (
            body["project"]["id"].as_str().unwrap().to_owned(),
            body["api_key"]["secret"].as_str().unwrap().to_owned(),
        )
    }

    /// Creates a key with `scopes` using `admin`; returns (key id, secret).
    pub async fn key(&self, admin: &str, scopes: &[&str]) -> (String, String) {
        let (status, body) = self
            .post(
                "/v1/keys",
                admin,
                None,
                json!({"label": "test key", "scopes": scopes}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (
            body["api_key"]["id"].as_str().unwrap().to_owned(),
            body["secret"].as_str().unwrap().to_owned(),
        )
    }

    pub async fn cleanup(self) {
        self.db.cleanup().await;
    }
}
