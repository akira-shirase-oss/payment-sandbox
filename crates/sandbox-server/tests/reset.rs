//! M7: a dataset reset opens a new epoch (D-022, D-066). It needs the `admin` scope, the
//! project's slug typed as the confirmation and the epoch the person saw; it deletes and
//! changes nothing of the earlier epoch, whose journal, payments and events stay archived.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use axum::http::StatusCode;
use common::{TestApp, TestSim};
use sandbox_db::ProjectScope;
use serde_json::{Value, json};
use sqlx::Connection as _;
use uuid::Uuid;

/// Every journal row of every epoch, as one digest, with the row counts.
async fn journal_fingerprint(app: &TestApp) -> (i64, i64, String) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM journal_entries),
                (SELECT count(*) FROM journal_lines),
                md5(coalesce((SELECT string_agg(e::text, '|' ORDER BY e.seq) FROM journal_entries e), '')
                    || coalesce((SELECT string_agg(l::text, '|' ORDER BY l.entry_id, l.line_no) FROM journal_lines l), ''))",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap()
}

async fn purchase(app: &TestApp, key: &str, suffix: &str) -> String {
    let (status, auth) = app
        .authorize(
            key,
            &format!("order-{suffix}"),
            4_200,
            "USD",
            "tok_sim_approve",
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{auth}");
    let pid = auth["payment"]["id"].as_str().unwrap().to_owned();
    let (status, body) = app
        .post(
            &format!("/v1/payments/{pid}/captures"),
            key,
            Some(&format!("capture-{suffix}")),
            json!({"amount": 4_200}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = app
        .post(
            &format!("/v1/payments/{pid}/refunds"),
            key,
            Some(&format!("refund-{suffix}")),
            json!({"amount": 1_200}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    pid
}

async fn reset(app: &TestApp, key: &str, body: Value) -> (StatusCode, Value) {
    app.post("/v1/project/reset", key, None, body).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_needs_admin_and_the_typed_slug_and_deletes_nothing() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (project_id, admin) = app.project("reset-shop").await;
    let (_, writer) = app.key(&admin, &["payments:write"]).await;
    let (_, reader) = app.key(&admin, &["read"]).await;
    let old_payment = purchase(&app, &writer, "1").await;
    let (_, before_balances) = app.get("/v1/balances", &reader).await;
    assert_ne!(before_balances["balances"], json!([]));
    let before = journal_fingerprint(&app).await;
    assert!(before.0 >= 3, "{before:?}");

    // Only admin; the confirmation must be the slug; the epoch must be the current one.
    for key in [&writer, &reader] {
        let (status, body) = reset(&app, key, json!({"confirm": "reset-shop", "epoch": 1})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    let (status, body) = reset(&app, &admin, json!({"confirm": "reset-sho", "epoch": 1})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "confirmation_mismatch");
    let (status, body) = reset(&app, &admin, json!({"confirm": "reset-shop", "epoch": 2})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "epoch_changed");
    let (status, body) = reset(&app, &admin, json!({"confirm": "reset-shop"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (_, project) = app.get("/v1/project", &reader).await;
    assert_eq!(
        project["project"]["dataset_epoch"], 1,
        "refusals change nothing"
    );

    let (status, body) = reset(&app, &admin, json!({"confirm": "reset-shop", "epoch": 1})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["reset"]["from_epoch"], 1);
    assert_eq!(body["reset"]["to_epoch"], 2);
    assert_eq!(body["reset"]["archived_payments"], 1);
    assert_eq!(body["project"]["dataset_epoch"], 2);
    // The same request again cannot reset twice.
    let (status, again) = reset(&app, &admin, json!({"confirm": "reset-shop", "epoch": 1})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(again["error"]["code"], "epoch_changed");
    assert!(
        again["error"]["message"]
            .as_str()
            .unwrap()
            .contains("epoch 2")
    );

    // Nothing of the journal changed; the archived epoch still reconciles.
    assert_eq!(journal_fingerprint(&app).await, before);
    let old_scope = ProjectScope {
        project_id: Uuid::parse_str(&project_id).unwrap(),
        epoch: 1,
    };
    let old = sandbox_db::reconcile::run(&app.db.pool, old_scope)
        .await
        .unwrap();
    assert!(old.is_clean(), "{old:?}");
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE project_id = $1 AND action = 'dataset.reset'",
    )
    .bind(old_scope.project_id)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(audits, 1);

    // Current reads start afresh; the old payment is archived, not missing.
    let (_, balances) = app.get("/v1/balances", &reader).await;
    assert_eq!(balances["balances"], json!([]));
    let (_, list) = app.get("/v1/payments", &reader).await;
    assert_eq!(list["payments"], json!([]));
    let (status, gone) = app
        .get(&format!("/v1/payments/{old_payment}"), &reader)
        .await;
    assert_eq!(status, StatusCode::GONE, "{gone}");
    assert_eq!(gone["error"]["code"], "dataset_archived");
    let (status, gone) = app
        .post(
            &format!("/v1/payments/{old_payment}/refunds"),
            &writer,
            Some("refund-after-reset"),
            json!({"amount": 100}),
        )
        .await;
    assert_eq!(status, StatusCode::GONE, "{gone}");
    let (status, missing) = app
        .get(&format!("/v1/payments/{}", Uuid::now_v7()), &reader)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");

    // The same keys start new operations: identity includes the epoch (D-015).
    let new_payment = purchase(&app, &writer, "1").await;
    assert_ne!(new_payment, old_payment);
    app.assert_reconciled(&reader).await;
    let (_, explained) = app
        .get("/v1/balances/USD/merchant_pending/explain", &reader)
        .await;
    assert_eq!(explained["balance_minor"], 3_000, "{explained}");
    app.cleanup().await;
    sim.remove().await;
}

/// The database itself refuses every change to a project but a reset, for the runtime role
/// and the owner alike.
#[tokio::test]
async fn the_database_lets_a_project_change_only_by_one_epoch_forward() {
    let app = TestApp::new(&[]).await;
    let (project_id, _) = app.project("guarded").await;
    let id = Uuid::parse_str(&project_id).unwrap();
    let mut runtime = sqlx::PgConnection::connect(&app.db.url()).await.unwrap();
    for (sql, code) in [
        (
            "UPDATE projects SET dataset_epoch = dataset_epoch - 1 WHERE id = $1",
            "PSP01",
        ),
        (
            "UPDATE projects SET dataset_epoch = dataset_epoch + 2 WHERE id = $1",
            "PSP01",
        ),
        ("UPDATE projects SET slug = 'stolen' WHERE id = $1", "42501"),
        ("DELETE FROM projects WHERE id = $1", "42501"),
    ] {
        let error = sqlx::query(sql)
            .bind(id)
            .execute(&mut runtime)
            .await
            .expect_err(sql);
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some(code),
            "{sql}: {error}"
        );
    }
    for (sql, code) in [
        (
            "UPDATE projects SET name = 'renamed', dataset_epoch = dataset_epoch + 1 WHERE id = $1",
            "PSP01",
        ),
        ("DELETE FROM projects WHERE id = $1", "PSJ01"),
    ] {
        let error = sqlx::query(sql)
            .bind(id)
            .execute(&app.db.pool)
            .await
            .expect_err(sql);
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some(code),
            "owner: {sql}: {error}"
        );
    }
    // The one allowed change.
    sqlx::query("UPDATE projects SET dataset_epoch = dataset_epoch + 1 WHERE id = $1")
        .bind(id)
        .execute(&mut runtime)
        .await
        .unwrap();
    runtime.close().await.unwrap();
    app.cleanup().await;
}

/// Two people confirming the same reset at once open one epoch, not two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_resets_of_one_epoch_open_exactly_one_new_epoch() {
    let app = TestApp::new(&[]).await;
    let (_, admin) = app.project("twice").await;
    let body = json!({"confirm": "twice", "epoch": 1});
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (router, admin, body) = (app.router.clone(), admin.clone(), body.clone());
        tasks.spawn(async move {
            common::raw(
                &router,
                axum::http::Method::POST,
                "/v1/project/reset",
                &[("authorization", &format!("Bearer {admin}"))],
                Some(body),
            )
            .await
            .0
        });
    }
    let mut statuses = Vec::new();
    while let Some(status) = tasks.join_next().await {
        statuses.push(status.unwrap());
    }
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        7
    );
    let (_, project) = app.get("/v1/project", &admin).await;
    assert_eq!(project["project"]["dataset_epoch"], 2);
    app.cleanup().await;
}

/// An operation still uncertain when the reset lands is resolved later, in its own epoch:
/// its money never appears in the new epoch, and the archived epoch stays balanced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_operation_open_across_a_reset_resolves_in_its_own_epoch() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[("PS_SIM_TIMEOUT_MS", "300")]).await;
    let (project_id, admin) = app.project("in-flight").await;
    let (status, body) = app
        .authorize(&admin, "lost-1", 2_500, "EUR", "tok_sim_lost_reply")
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["operation"]["state"], "uncertain");
    let (status, body) = reset(&app, &admin, json!({"confirm": "in-flight", "epoch": 1})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let report = sandbox_server::reconciler::sweep(&app.state).await.unwrap();
    assert_eq!(report.resolved_from_record, 1, "{report:?}");
    let old_scope = ProjectScope {
        project_id: Uuid::parse_str(&project_id).unwrap(),
        epoch: 1,
    };
    let old = sandbox_db::reconcile::run(&app.db.pool, old_scope)
        .await
        .unwrap();
    assert!(old.is_clean(), "{old:?}");
    let entries_in_old: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM journal_entries WHERE project_id = $1 AND dataset_epoch = 1",
    )
    .bind(old_scope.project_id)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(entries_in_old, 1);
    let (_, balances) = app.get("/v1/balances", &admin).await;
    assert_eq!(balances["balances"], json!([]));
    app.assert_reconciled(&admin).await;
    app.cleanup().await;
    sim.remove().await;
}

/// A cursor seals its epoch (D-056), and a reset closes the archived epoch's checkouts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cursors_and_checkouts_of_an_archived_epoch_are_refused() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, admin) = app.project("cursor-shop").await;
    purchase(&app, &admin, "a").await;
    let (status, page) = app
        .get("/v1/balances/USD/payer_funding/explain?limit=1", &admin)
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let cursor = page["explanation"]["next"]["cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, session) = app
        .post(
            "/v1/checkout_sessions",
            &admin,
            None,
            json!({"amount": 1_000, "currency": "USD"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let session_id = Uuid::parse_str(session["session"]["id"].as_str().unwrap()).unwrap();
    assert!(
        !sandbox_db::checkout::find(&app.db.pool, session_id)
            .await
            .unwrap()
            .unwrap()
            .expired
    );

    let (status, body) = reset(&app, &admin, json!({"confirm": "cursor-shop", "epoch": 1})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, refused) = app
        .get(
            &format!("/v1/balances/USD/payer_funding/explain?limit=1&cursor={cursor}"),
            &admin,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert_eq!(refused["error"]["code"], "invalid_cursor");
    let closed = sandbox_db::checkout::find(&app.db.pool, session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        closed.expired,
        "a reset closes the archived epoch's sessions"
    );
    assert!(!closed.can_start_new());
    app.cleanup().await;
    sim.remove().await;
}
