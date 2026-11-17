//! M5: every synthetic balance can be explained from journal entries (the ship gate's second
//! half), and reconciliation detects a planted drift.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

mod common;

use axum::http::StatusCode;
use common::{TestApp, TestSim};
use serde_json::json;

async fn purchases(app: &TestApp, key: &str) -> Vec<String> {
    let mut pids = Vec::new();
    for (n, (amount, capture, refund)) in [
        (1_000, 600, 100),
        (500, 500, 0),
        (250, 0, 0),
        (900, 300, 300),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, auth) = app
            .authorize(key, &format!("o{n}"), amount, "USD", "tok_sim_approve")
            .await;
        let pid = auth["payment"]["id"].as_str().unwrap().to_owned();
        if capture > 0 {
            app.post(
                &format!("/v1/payments/{pid}/captures"),
                key,
                Some(&format!("c{n}")),
                json!({"amount": capture}),
            )
            .await;
        }
        if refund > 0 {
            app.post(
                &format!("/v1/payments/{pid}/refunds"),
                key,
                Some(&format!("r{n}")),
                json!({"amount": refund}),
            )
            .await;
        }
        pids.push(pid);
    }
    pids
}

#[tokio::test]
async fn every_balance_is_explained_line_by_line_across_pages() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    purchases(&app, &key).await;
    let (_, balances) = app.get("/v1/balances", &key).await;
    let balances = balances["balances"].as_array().unwrap().clone();
    assert_eq!(balances.len(), 3);
    for balance in balances {
        let account = balance["account"].as_str().unwrap();
        let mut cursor = String::new();
        let mut sum = 0_i64;
        let mut lines = 0_i64;
        let mut last_running = None;
        let mut pages = 0;
        loop {
            let (status, page) = app
                .get(
                    &format!("/v1/balances/USD/{account}/explain?limit=3{cursor}"),
                    &key,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert_eq!(page["balance_minor"], balance["balance_minor"], "{account}");
            for line in page["explanation"]["lines"].as_array().unwrap() {
                sum += line["amount_minor"].as_i64().unwrap();
                lines += 1;
                assert_eq!(
                    line["running_minor"].as_i64().unwrap(),
                    sum,
                    "the running total is the sum so far"
                );
                last_running = Some(sum);
            }
            pages += 1;
            let next = &page["explanation"]["next"];
            if next.is_object() {
                cursor = format!("&cursor={}", next["cursor"].as_str().unwrap());
            } else {
                assert_eq!(page["explanation"]["line_count"].as_i64().unwrap(), lines);
                break;
            }
        }
        assert!(pages > 1, "{account}: paged");
        assert_eq!(Some(sum), last_running);
        assert_eq!(
            sum,
            balance["signed_minor"].as_i64().unwrap(),
            "{account}: the lines sum to the balance"
        );
    }
    let (status, _) = app.get("/v1/balances/GBP/payer_held/explain", &key).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app.get("/v1/balances/USD/cash/explain", &key).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    app.cleanup().await;
    sim.remove().await;
}

/// A planted drift — a payment's committed sums changed behind the ledger's back (raw SQL
/// that the CHECK constraints still allow) — is found exactly, and only it.
#[tokio::test]
async fn a_planted_drift_is_detected_exactly() {
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    let pids = purchases(&app, &key).await;
    app.assert_reconciled(&key).await;
    sqlx::query("UPDATE payments SET refunded_minor = refunded_minor + 50 WHERE id = $1::uuid")
        .bind(&pids[0])
        .execute(&app.db.pool)
        .await
        .unwrap();
    let (_, report) = app.get("/v1/reconciliation", &key).await;
    assert_eq!(report["clean"], false, "{report}");
    assert_eq!(report["report"]["payment_mismatches"], json!([pids[0]]));
    let accounts: Vec<String> = report["report"]["account_mismatches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m[1].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(accounts, ["merchant_pending", "payer_funding"], "{report}");
    sqlx::query("UPDATE payments SET refunded_minor = refunded_minor - 50 WHERE id = $1::uuid")
        .bind(&pids[0])
        .execute(&app.db.pool)
        .await
        .unwrap();
    app.assert_reconciled(&key).await;
    app.cleanup().await;
    sim.remove().await;
}

/// Review finding 5 (experiment X1): an entry that allocated its sequence number earlier but
/// commits later must not make paging skip it. Every page of one explanation reports the
/// same balance, and the lines of all pages sum exactly to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explain_pages_stay_consistent_while_entries_commit_out_of_order() {
    use sandbox_db::journal::{self, EntryRefs};
    use sandbox_domain::identity::entry_id;
    use sandbox_domain::{Amount, Currency, Recipe};
    let sim = TestSim::start().await;
    let app = TestApp::with_sim(&sim, &[]).await;
    let (_, key) = app.project("store").await;
    let pids = purchases(&app, &key).await;
    let (_, project) = app.get("/v1/project", &key).await;
    let scope = sandbox_db::ProjectScope {
        project_id: project["project"]["id"].as_str().unwrap().parse().unwrap(),
        epoch: 1,
    };
    let (_, before) = app
        .get("/v1/balances/USD/merchant_pending/explain?limit=1000", &key)
        .await;
    let committed = before["explanation"]["line_count"].as_i64().unwrap();

    // A posting allocates its sequence number now and commits later.
    let mut held = app.db.pool.begin().await.unwrap();
    let entry = Recipe::Capture
        .post(Currency::Usd, Amount::new(200).unwrap())
        .unwrap();
    journal::post(
        &mut held,
        scope,
        entry_id(uuid::Uuid::now_v7(), Recipe::Capture),
        &entry,
        EntryRefs::default(),
    )
    .await
    .unwrap();
    // Meanwhile a later posting commits.
    let (status, _) = app
        .post(
            &format!("/v1/payments/{}/refunds", pids[1]),
            &key,
            Some("late"),
            json!({"amount": 50}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // Pages of two lines: the explanation must page several times across the late commit
    // (review N12: with one page this test passed even without D-042's fix).
    let page_size = 2;
    let first = format!("/v1/balances/USD/merchant_pending/explain?limit={page_size}");
    let (_, page) = app.get(&first, &key).await;
    held.commit().await.unwrap();
    let balance = page["explanation"]["balance_minor"].as_i64().unwrap();
    let mut sum: i64 = page["explanation"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["amount_minor"].as_i64().unwrap())
        .sum();
    let mut cursor = page["explanation"]["next"].clone();
    let mut pages = 1;
    for _ in 0..100 {
        let path = if cursor.is_object() {
            format!(
                "/v1/balances/USD/merchant_pending/explain?limit={page_size}&cursor={}",
                cursor["cursor"].as_str().unwrap()
            )
        } else {
            break;
        };
        pages += 1;
        let (_, page) = app.get(&path, &key).await;
        assert_eq!(
            page["explanation"]["balance_minor"].as_i64().unwrap(),
            balance,
            "the balance moved between pages: {page}"
        );
        sum += page["explanation"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["amount_minor"].as_i64().unwrap())
            .sum::<i64>();
        cursor = page["explanation"]["next"].clone();
    }
    assert!(
        i64::from(pages) * page_size > committed,
        "only {pages} page(s) of {page_size} were read for {committed} lines"
    );
    assert!(
        pages >= 2,
        "a pager that never reads a second page proves nothing"
    );
    assert_eq!(
        sum, balance,
        "the pages' lines must sum to the balance they report"
    );
    // A fresh explanation sees both new postings.
    let (_, fresh) = app
        .get("/v1/balances/USD/merchant_pending/explain?limit=1000", &key)
        .await;
    assert_eq!(
        fresh["explanation"]["line_count"].as_i64().unwrap(),
        committed + 2
    );
    app.cleanup().await;
    sim.remove().await;
}

/// Review finding 16: after a restore, the sequence restarts at the backup's value, so the
/// numbers after the backup point are handed out again to other entries, and the first
/// page's snapshot belongs to another history. An explanation cursor names the database's
/// restore generation; one from before a restore is refused, never silently continued.
#[tokio::test]
async fn an_explanation_cursor_from_before_a_restore_is_refused() {
    let sim = TestSim::start().await;
    let vars = common::sim_vars(&sim, &[]);
    let first = TestApp::on(sandbox_db::testing::TestDb::new().await, &vars);
    let (_, key) = first.project("store").await;
    for n in 0..3 {
        let (status, _) = first
            .authorize(&key, &format!("g-{n}"), 100, "USD", "tok_sim_approve")
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let path = "/v1/balances/USD/payer_held/explain?limit=1";
    let (status, page) = first.get(path, &key).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let next = page["explanation"]["next"].as_object().unwrap().clone();
    let mut cursor = String::new();
    for (k, v) in &next {
        let v = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
        cursor.extend(["&", k.as_str(), "=", v.as_str()]);
    }
    let (status, _) = first.get(&format!("{path}{cursor}"), &key).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the cursor works where it was issued"
    );

    first.state.db.close().await;
    let common::TestApp { db: mut live, .. } = first;
    let backup = live.snapshot().await;
    let restored = TestApp::on(backup, &vars);
    let (status, body) = restored.get(&format!("{path}{cursor}"), &key).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "stale_cursor", "{body}");
    let (status, fresh) = restored.get(path, &key).await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    assert!(fresh["explanation"]["generation"].is_string(), "{fresh}");
    assert_ne!(
        fresh["explanation"]["generation"],
        page["explanation"]["generation"]
    );
    restored.cleanup().await;
    live.cleanup().await;
    sim.remove().await;
}

/// Review N3: the cursor's `as_of` went straight into a `::pg_snapshot` cast, so a card
/// number there became a 500 whose message (with the number) reached the ERROR log and the
/// database's log, and `running` was whatever the client said. Cursors are now issued
/// opaque and sealed by the server and checked before any query; card numbers anywhere in
/// a query string are refused like everywhere else.
#[tokio::test]
async fn a_cursor_the_server_did_not_issue_is_refused_before_any_query() {
    let app = TestApp::new(&[]).await;
    let (_, key) = app.project("store").await;
    let base = "/v1/balances/USD/payer_held/explain";
    let (status, body) = app.get(&format!("{base}?cursor=abcdef"), &key).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_cursor", "{body}");
    let (status, body) = app
        .get(
            &format!("{base}?generation=1.1.1&as_of=4242424242424242&after_seq=0&running=0"),
            &key,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "raw_card_data_rejected", "{body}");
    let (status, body) = app
        .get("/v1/payments?description=4242%204242%204242%204242", &key)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    app.cleanup().await;
}
