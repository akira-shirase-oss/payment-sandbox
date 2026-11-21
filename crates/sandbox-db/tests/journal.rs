//! The journal's rules, proved against a real PostgreSQL: the schema itself refuses
//! unbalanced, foreign, late and mutating writes, whatever the application does.
//!
//! Needs `PS_TEST_DATABASE_URL` (`scripts/stack.sh up`, then `eval "$(scripts/stack.sh env)"`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure report"
)]

use std::collections::BTreeMap;

use proptest::prelude::*;
use sandbox_db::journal::{self, EntryRefs};
use sandbox_db::reconcile;
use sandbox_db::testing::TestDb;
use sandbox_db::{DbError, ProjectScope, projects};
use sandbox_domain::identity::entry_id;
use sandbox_domain::{AccountKind, Amount, Currency, Recipe};
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

async fn project(pool: &PgPool, slug: &str) -> ProjectScope {
    projects::create(pool, Uuid::now_v7(), slug, "Test store")
        .await
        .unwrap()
        .scope()
}

async fn post(
    pool: &PgPool,
    scope: ProjectScope,
    recipe: Recipe,
    currency: Currency,
    minor: i64,
) -> Uuid {
    let entry = recipe.post(currency, Amount::new(minor).unwrap()).unwrap();
    let id = entry_id(Uuid::now_v7(), recipe);
    let mut tx = pool.begin().await.unwrap();
    journal::post(&mut tx, scope, id, &entry, EntryRefs::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

/// Runs raw SQL in its own transaction and returns the SQLSTATE it failed with.
async fn sqlstate_of(pool: &PgPool, sql: &str) -> String {
    let mut tx = pool.begin().await.unwrap();
    let result = sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(&mut *tx)
        .await;
    let result = match result {
        Ok(_) => tx.commit().await,
        Err(e) => Err(e),
    };
    let error = result.expect_err("the database must refuse this");
    let code = error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned);
    code.unwrap_or_else(|| panic!("no SQLSTATE: {error}"))
}

#[tokio::test]
async fn a_balanced_entry_commits_and_balances_are_its_sums() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-a").await;
    post(&db.pool, scope, Recipe::Authorize, Currency::Usd, 500).await;
    post(&db.pool, scope, Recipe::Capture, Currency::Usd, 200).await;
    let balances = journal::balances(&db.pool, scope).await.unwrap();
    let get = |k: AccountKind| {
        balances
            .iter()
            .find(|b| b.account == k && b.currency == Currency::Usd)
            .map_or(0, |b| k.presented(b.signed_minor))
    };
    assert_eq!(get(AccountKind::PayerFunding), 500);
    assert_eq!(get(AccountKind::PayerHeld), 300);
    assert_eq!(get(AccountKind::MerchantPending), 200);
    let imbalance = journal::imbalance_by_currency(&db.pool, scope)
        .await
        .unwrap();
    assert_eq!(imbalance, Vec::<(String, i64)>::new());
    db.cleanup().await;
}

#[tokio::test]
async fn updates_deletes_and_truncates_of_the_journal_are_refused_by_the_database() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-a").await;
    post(&db.pool, scope, Recipe::Authorize, Currency::Eur, 900).await;
    for sql in [
        "UPDATE journal_lines SET amount_minor = amount_minor * 2",
        "DELETE FROM journal_lines",
        "TRUNCATE journal_lines",
        "UPDATE journal_entries SET recipe = 'capture'",
        "DELETE FROM journal_entries",
        "TRUNCATE journal_entries CASCADE",
        "TRUNCATE projects CASCADE",
    ] {
        assert_eq!(sqlstate_of(&db.pool, sql).await, "PSJ01", "{sql}");
    }
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM journal_lines")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(lines, 2, "nothing was changed");
    db.cleanup().await;
}

#[tokio::test]
async fn unbalanced_single_and_late_lines_are_refused_even_from_raw_sql() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-a").await;
    let p = scope.project_id;
    let entry = |id: &str| {
        format!(
            "INSERT INTO journal_entries (project_id, id, dataset_epoch, currency, recipe, semantics_version)
             VALUES ('{p}', '{id}', 1, 'USD', 'authorize', 1);"
        )
    };
    let e1 = Uuid::now_v7();
    let unbalanced = format!(
        "{} INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
         VALUES ('{p}', '{e1}', 1, 'USD', 1, 'payer_funding', 500), ('{p}', '{e1}', 2, 'USD', 1, 'payer_held', -499);",
        entry(&e1.to_string())
    );
    assert_eq!(sqlstate_of(&db.pool, &unbalanced).await, "PSJ02");

    let e2 = Uuid::now_v7();
    let single = format!(
        "{} INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
         VALUES ('{p}', '{e2}', 1, 'USD', 1, 'payer_funding', 500);",
        entry(&e2.to_string())
    );
    assert_eq!(sqlstate_of(&db.pool, &single).await, "PSJ02");

    let e3 = Uuid::now_v7();
    let no_lines = entry(&e3.to_string());
    assert_eq!(
        sqlstate_of(&db.pool, &no_lines).await,
        "PSJ03",
        "checked at commit"
    );

    // A committed entry can never gain lines, not even a balanced pair.
    let posted = post(&db.pool, scope, Recipe::Authorize, Currency::Usd, 100).await;
    let late = format!(
        "INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
         VALUES ('{p}', '{posted}', 3, 'USD', 1, 'payer_funding', 7), ('{p}', '{posted}', 4, 'USD', 1, 'payer_held', -7);"
    );
    assert_eq!(sqlstate_of(&db.pool, &late).await, "PSJ02");
    db.cleanup().await;
}

#[tokio::test]
async fn currencies_accounts_and_projects_cannot_be_mixed() {
    let db = TestDb::new().await;
    let a = project(&db.pool, "store-a").await;
    let b = project(&db.pool, "store-b").await;
    let (pa, pb) = (a.project_id, b.project_id);
    let e = Uuid::now_v7();
    let header = format!(
        "INSERT INTO journal_entries (project_id, id, dataset_epoch, currency, recipe, semantics_version)
         VALUES ('{pa}', '{e}', 1, 'USD', 'authorize', 1);"
    );
    let cases = [
        // A EUR line on a USD entry: the composite foreign key refuses it.
        format!(
            "{header} INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
             VALUES ('{pa}', '{e}', 1, 'USD', 1, 'payer_funding', 5), ('{pa}', '{e}', 2, 'EUR', 1, 'payer_held', -5);"
        ),
        // An account outside the chart.
        format!(
            "{header} INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
             VALUES ('{pa}', '{e}', 1, 'USD', 1, 'payer_funding', 5), ('{pa}', '{e}', 2, 'USD', 1, 'cash', -5);"
        ),
        // Lines of project B pointing at project A's entry.
        format!(
            "{header} INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
             VALUES ('{pb}', '{e}', 1, 'USD', 1, 'payer_funding', 5), ('{pb}', '{e}', 2, 'USD', 1, 'payer_held', -5);"
        ),
        // A currency outside the set.
        format!(
            "INSERT INTO journal_entries (project_id, id, dataset_epoch, currency, recipe, semantics_version)
             VALUES ('{pa}', '{e}', 1, 'GBP', 'authorize', 1);"
        ),
    ];
    for sql in cases {
        let state = sqlstate_of(&db.pool, &sql).await;
        assert!(state == "23503" || state == "23514", "{state} for {sql}");
    }
    db.cleanup().await;
}

#[tokio::test]
async fn posting_time_comes_from_the_database_clock_not_the_caller() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-a").await;
    let p = scope.project_id;
    let e = Uuid::now_v7();
    sqlx::raw_sql(AssertSqlSafe(format!(
        "BEGIN;
         INSERT INTO journal_entries (project_id, id, dataset_epoch, currency, recipe, semantics_version, posted_at)
         VALUES ('{p}', '{e}', 1, 'JPY', 'authorize', 1, '1999-01-01T00:00:00Z');
         INSERT INTO journal_lines (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
         VALUES ('{p}', '{e}', 1, 'JPY', 1, 'payer_funding', 5), ('{p}', '{e}', 2, 'JPY', 1, 'payer_held', -5);
         COMMIT;"
    )))
    .execute(&db.pool)
    .await
    .unwrap();
    let year: f64 = sqlx::query_scalar(
        "SELECT extract(year FROM posted_at)::float8 FROM journal_entries WHERE id = $1",
    )
    .bind(e)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(year >= 2026.0, "a backdated posted_at was accepted: {year}");
    db.cleanup().await;
}

#[tokio::test]
async fn a_transaction_that_dies_mid_posting_leaves_nothing_behind() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-a").await;
    let entry = Recipe::Authorize
        .post(Currency::Kwd, Amount::new(1_234).unwrap())
        .unwrap();
    {
        let mut tx = db.pool.begin().await.unwrap();
        journal::post(&mut tx, scope, Uuid::now_v7(), &entry, EntryRefs::default())
            .await
            .unwrap();
        // Dropped without commit: what a crash between the posting and the commit does.
    }
    let mut conn = db.pool.acquire().await.unwrap();
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM journal_lines")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(lines, 0);
    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_correction_is_a_reversing_entry_and_both_stay_in_history() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-a").await;
    let entry = Recipe::Capture
        .post(Currency::Usd, Amount::new(250).unwrap())
        .unwrap();
    let original = entry_id(Uuid::now_v7(), Recipe::Capture);
    let mut tx = db.pool.begin().await.unwrap();
    journal::post(&mut tx, scope, original, &entry, EntryRefs::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    let reversal = entry.reversal();
    journal::post(
        &mut tx,
        scope,
        Uuid::now_v7(),
        &reversal,
        EntryRefs {
            reverses: Some(original),
            ..EntryRefs::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let balances = journal::balances(&db.pool, scope).await.unwrap();
    assert!(balances.iter().all(|b| b.signed_minor == 0), "{balances:?}");
    let entries: i64 = sqlx::query_scalar("SELECT count(*) FROM journal_entries")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(entries, 2);
    // A reversal must name what it reverses.
    let mut tx = db.pool.begin().await.unwrap();
    let orphan = journal::post(
        &mut tx,
        scope,
        Uuid::now_v7(),
        &reversal,
        EntryRefs::default(),
    )
    .await;
    assert!(matches!(orphan, Err(DbError::Constraint(_))), "{orphan:?}");
    drop(tx);
    db.cleanup().await;
}

/// The derived balances (D-096) are per epoch: a reset's new epoch starts empty and never
/// mixes with the archived epoch's sums, which stay as they were.
#[tokio::test]
async fn derived_balances_are_kept_per_epoch() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-epochs").await;
    post(&db.pool, scope, Recipe::Authorize, Currency::Usd, 500).await;
    sqlx::query("UPDATE projects SET dataset_epoch = dataset_epoch + 1 WHERE id = $1")
        .bind(scope.project_id)
        .execute(&db.pool)
        .await
        .unwrap();
    let scope2 = ProjectScope {
        project_id: scope.project_id,
        epoch: 2,
    };
    post(&db.pool, scope2, Recipe::Authorize, Currency::Usd, 700).await;
    let get = |balances: &[journal::Balance], account: AccountKind| {
        balances
            .iter()
            .find(|b| b.account == account && b.currency == Currency::Usd)
            .map_or(0, |b| b.signed_minor)
    };
    let epoch1 = journal::balances(&db.pool, scope).await.unwrap();
    let epoch2 = journal::balances(&db.pool, scope2).await.unwrap();
    assert_eq!(get(&epoch1, AccountKind::PayerFunding), 500);
    assert_eq!(get(&epoch1, AccountKind::PayerHeld), -500);
    assert_eq!(get(&epoch2, AccountKind::PayerFunding), 700);
    assert_eq!(get(&epoch2, AccountKind::PayerHeld), -700);
    // Both epochs' derived balances re-derive exactly from their own journals (these
    // posts have no payment rows, so the payments-implied check is not the measure here).
    for epoch in [scope, scope2] {
        let report = reconcile::run(&db.pool, epoch).await.unwrap();
        assert!(
            report.balance_mismatches.is_empty(),
            "epoch {}: {:?}",
            epoch.epoch,
            report.balance_mismatches
        );
    }
    db.cleanup().await;
}

/// A balance row that was drifted outside the posting path (D-096) is reported by
/// reconciliation — the journal stays the truth it is measured against, and the drift is
/// a finding, never silently repaired or trusted.
#[tokio::test]
async fn a_drifted_derived_balance_is_reported_not_trusted() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-drift").await;
    post(&db.pool, scope, Recipe::Authorize, Currency::Usd, 500).await;
    sqlx::query(
        "UPDATE account_balances SET balance_minor = balance_minor + 1
          WHERE project_id = $1 AND account_kind = 'payer_funding'",
    )
    .bind(scope.project_id)
    .execute(&db.pool)
    .await
    .unwrap();
    let report = reconcile::run(&db.pool, scope).await.unwrap();
    assert!(!report.is_clean());
    assert_eq!(
        report.balance_mismatches,
        vec![("USD".to_string(), "payer_funding".to_string(), 500, 501)]
    );
    // The ledger itself is untouched: the currency's journal lines still sum to zero —
    // only the derivation drifted.
    assert_eq!(report.imbalanced_currencies, Vec::<(String, i64)>::new());
    // The read path serves the derived number; the drift is caught by the re-derivation,
    // not hidden by re-summing the journal on read.
    let balances = journal::balances(&db.pool, scope).await.unwrap();
    let funding = balances
        .iter()
        .find(|b| b.account == AccountKind::PayerFunding)
        .unwrap();
    assert_eq!(funding.signed_minor, 501);
    db.cleanup().await;
}

/// Postings on separate transactions racing the same accounts leave the derived balances
/// exactly equal to the journal's sums: the conflicting upserts serialize on the account
/// row and every delta lands once.
#[tokio::test]
async fn concurrent_postings_keep_the_derived_balances_exact() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-race").await;
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let pool = db.pool.clone();
            tokio::spawn(async move {
                for _ in 0..5 {
                    let entry = Recipe::Authorize
                        .post(Currency::Usd, Amount::new(100).unwrap())
                        .unwrap();
                    let mut tx = pool.begin().await.unwrap();
                    journal::post(&mut tx, scope, Uuid::now_v7(), &entry, EntryRefs::default())
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                }
            })
        })
        .collect();
    for task in tasks {
        task.await.unwrap();
    }
    let balances = journal::balances(&db.pool, scope).await.unwrap();
    let funding = balances
        .iter()
        .find(|b| b.account == AccountKind::PayerFunding && b.currency == Currency::Usd)
        .unwrap();
    assert_eq!(funding.signed_minor, 8 * 5 * 100);
    let report = reconcile::run(&db.pool, scope).await.unwrap();
    assert!(
        report.balance_mismatches.is_empty(),
        "{:?}",
        report.balance_mismatches
    );
    db.cleanup().await;
}

/// Postings across currencies and their reversals racing each other cannot deadlock: the
/// upsert orders the balance rows by (currency, `account_kind`, stripe) in its SELECT, so
/// every posting takes the locks in the same order whatever its deltas are. Entries carry
/// one currency by schema, so the two-currency case is approached by racing every
/// currency at once; a deadlock would surface as a `PgDatabaseError`, not a wrong sum.
#[tokio::test]
async fn ordered_upserts_cannot_deadlock_across_currencies_and_reversals() {
    let db = TestDb::new().await;
    let scope = project(&db.pool, "store-order").await;
    let mut handles = Vec::new();
    for round in 0..8 {
        for currency in Currency::ALL {
            let pool = db.pool.clone();
            handles.push(tokio::spawn(async move {
                let entry = Recipe::Capture
                    .post(currency, Amount::new(10 + round).unwrap())
                    .unwrap();
                let original = Uuid::now_v7();
                let mut tx = pool.begin().await.unwrap();
                journal::post(&mut tx, scope, original, &entry, EntryRefs::default())
                    .await
                    .unwrap();
                journal::post(
                    &mut tx,
                    scope,
                    Uuid::now_v7(),
                    &entry.reversal(),
                    EntryRefs {
                        reverses: Some(original),
                        ..EntryRefs::default()
                    },
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }));
        }
    }
    for handle in handles {
        handle.await.unwrap();
    }
    let report = reconcile::run(&db.pool, scope).await.unwrap();
    assert!(
        report.balance_mismatches.is_empty(),
        "{:?}",
        report.balance_mismatches
    );
    db.cleanup().await;
}

/// The property: for random recipe postings across currencies, every committed balance
/// equals the sum the postings imply, and every currency sums to zero. The read goes
/// through the derived balances (D-096), so this is also the derived-equals-journal
/// property.
#[test]
fn random_postings_always_balance_and_match_their_model() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let db = runtime.block_on(TestDb::new());
    let scope = runtime.block_on(project(&db.pool, "store-p"));
    let model: std::cell::RefCell<BTreeMap<(Currency, AccountKind), i64>> =
        std::cell::RefCell::default();
    let strategy = prop::collection::vec((0usize..4, 0usize..4, 1i64..1_000_000), 1..20);
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(16));
    runner
        .run(&strategy, |postings| {
            runtime.block_on(async {
                let mut tx = db.pool.begin().await.unwrap();
                for (r, c, minor) in &postings {
                    let recipe = Recipe::ALL[*r];
                    let currency = Currency::ALL[*c];
                    let entry = recipe.post(currency, Amount::new(*minor).unwrap()).unwrap();
                    journal::post(&mut tx, scope, Uuid::now_v7(), &entry, EntryRefs::default())
                        .await
                        .unwrap();
                    for line in entry.lines() {
                        *model
                            .borrow_mut()
                            .entry((currency, line.account))
                            .or_default() += line.signed_minor();
                    }
                }
                tx.commit().await.unwrap();
            });
            let stored = runtime
                .block_on(journal::balances(&db.pool, scope))
                .unwrap();
            for balance in &stored {
                let expected = model
                    .borrow()
                    .get(&(balance.currency, balance.account))
                    .copied()
                    .unwrap_or(0);
                prop_assert_eq!(balance.signed_minor, expected);
            }
            for currency in Currency::ALL {
                let total: i64 = stored
                    .iter()
                    .filter(|b| b.currency == currency)
                    .map(|b| b.signed_minor)
                    .sum();
                prop_assert_eq!(total, 0);
            }
            let imbalance = runtime
                .block_on(journal::imbalance_by_currency(&db.pool, scope))
                .unwrap();
            prop_assert!(imbalance.is_empty());
            Ok(())
        })
        .unwrap();
    runtime.block_on(db.cleanup());
}

/// S1-1's second half: a cached identity's epoch can lag a reset by the notification
/// latency (the key cache's 30 s TTL before the listener lands). A write begun under
/// the archived epoch is refused by the transaction itself — the scope's epoch is
/// checked against `projects.dataset_epoch` under a key-share lock, never trusted
/// from whatever handed the scope out.
#[tokio::test]
async fn a_write_scoped_to_an_archived_epoch_is_refused() {
    use sandbox_db::payments;
    use sandbox_domain::Amount;

    let db = TestDb::new().await;
    let scope = project(&db.pool, "epoch-guard").await;
    let new = |key: &'static str| payments::NewAuthorize {
        operation_id: Uuid::now_v7(),
        payment_id: Uuid::now_v7(),
        idempotency_key: key,
        fingerprint: [u8::try_from(key.len()).unwrap(); 32],
        amount: Amount::new(100).unwrap(),
        currency: Currency::Usd,
        token: "tok_sim_approve",
        description: None,
    };
    payments::begin_authorize(&db.pool, scope, &new("epoch-ok"))
        .await
        .expect("a write in the current epoch must begin");

    let reset = projects::reset(
        &db.pool,
        scope.project_id,
        scope.epoch,
        "epoch-guard",
        "test",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(
        matches!(reset, projects::ResetOutcome::Reset { .. }),
        "{reset:?}"
    );

    // The stale scope — the epoch a lagging cache still hands out — is refused, and
    // the refusal is the named one, not a constraint or a not-found.
    let err = payments::begin_authorize(&db.pool, scope, &new("epoch-stale"))
        .await
        .expect_err("a stale epoch must never write");
    assert!(
        matches!(err, payments::BeginError::Db(DbError::EpochMoved)),
        "{err:?}"
    );
    // And nothing of it persisted: the archived epoch holds only the first payment.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM payments WHERE project_id = $1 AND dataset_epoch = $2",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    db.cleanup().await;
}
