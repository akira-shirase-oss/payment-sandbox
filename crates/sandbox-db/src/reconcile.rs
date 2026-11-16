//! Reconciliation: prove the derived records agree with the journal (D-003, D-028).
//!
//! - every currency's journal lines sum to zero;
//! - every payment's committed sums equal the totals of its journal entries;
//! - every succeeded operation has exactly one entry, and every entry's operation succeeded.
//!
//! A mismatch is a defect, never a rounding error.

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{DbError, ProjectScope};

/// The outcome of one reconciliation pass over a project's current epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Currencies whose lines do not sum to zero, with the residue.
    pub imbalanced_currencies: Vec<(String, i64)>,
    /// Payments whose sums differ from their journal entries.
    pub payment_mismatches: Vec<Uuid>,
    /// Succeeded operations without exactly one entry, plus entries whose operation did
    /// not succeed.
    pub operation_entry_mismatches: i64,
    /// Accounts whose journal balance differs from what the payments' committed sums
    /// imply: `(currency, account, journal, implied)`.
    pub account_mismatches: Vec<(String, String, i64, i64)>,
    /// Operations still pending or uncertain.
    pub open_operations: i64,
    /// Payments checked.
    pub payments_checked: i64,
    /// Journal entries checked.
    pub entries_checked: i64,
}

impl Report {
    /// True when nothing disagrees.
    pub fn is_clean(&self) -> bool {
        self.imbalanced_currencies.is_empty()
            && self.payment_mismatches.is_empty()
            && self.operation_entry_mismatches == 0
            && self.account_mismatches.is_empty()
    }
}

/// The deadline of a reconciliation pass: a report scans the whole epoch, so it gets more
/// than the hot path's per-statement deadline, but still a bound.
pub const DEADLINE: &str = "30s";

/// Runs every check for `scope`, in one read-only transaction (one consistent snapshot)
/// with its own statement deadline.
pub async fn run(pool: &PgPool, scope: ProjectScope) -> Result<Report, DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(DEADLINE)
        .execute(&mut *tx)
        .await?;
    let imbalanced_currencies: Vec<(String, i64)> = sqlx::query_as(
        "SELECT currency, sum(amount_minor)::bigint AS total
           FROM journal_lines
          WHERE project_id = $1 AND dataset_epoch = $2
          GROUP BY currency
         HAVING sum(amount_minor) <> 0",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_all(&mut *tx)
    .await?;
    // Index-driven per payment (payments, then journal_entries_by_payment, then the lines'
    // primary key), so the plan does not depend on fresh statistics: a just-written project
    // otherwise invites a nested loop over sequential scans.
    let payment_mismatches: Vec<Uuid> = sqlx::query_scalar(
        "SELECT p.id FROM payments p
          CROSS JOIN LATERAL (
              SELECT coalesce(sum(t.total) FILTER (WHERE t.recipe = 'authorize'), 0) AS authorized,
                     coalesce(sum(t.total) FILTER (WHERE t.recipe = 'capture'), 0) AS captured,
                     coalesce(sum(t.total) FILTER (WHERE t.recipe = 'void'), 0) AS voided,
                     coalesce(sum(t.total) FILTER (WHERE t.recipe = 'refund'), 0) AS refunded
                FROM (SELECT e.recipe,
                             (SELECT sum(l.amount_minor) FROM journal_lines l
                               WHERE l.project_id = e.project_id AND l.entry_id = e.id
                                 AND l.amount_minor > 0) AS total
                        FROM journal_entries e
                       WHERE e.project_id = p.project_id AND e.payment_id = p.id) t
          ) s
          WHERE p.project_id = $1 AND p.dataset_epoch = $2
            AND (p.authorized_minor <> s.authorized OR p.captured_minor <> s.captured
                 OR p.voided_minor <> s.voided OR p.refunded_minor <> s.refunded)
          ORDER BY p.id LIMIT 1000",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_all(&mut *tx)
    .await?;
    let row = sqlx::query(
        "SELECT
            (SELECT count(*) FROM operations o
              WHERE o.project_id = $1 AND o.dataset_epoch = $2 AND o.state = 'succeeded'
                AND (SELECT count(*) FROM journal_entries e
                      WHERE e.project_id = o.project_id AND e.operation_id = o.id) <> 1)
          + (SELECT count(*) FROM journal_entries e
               JOIN operations o ON o.project_id = e.project_id AND o.id = e.operation_id
              WHERE e.project_id = $1 AND e.dataset_epoch = $2 AND o.state <> 'succeeded') AS mismatches,
            (SELECT count(*) FROM operations
              WHERE project_id = $1 AND dataset_epoch = $2 AND state IN ('pending', 'uncertain')) AS open,
            (SELECT count(*) FROM payments WHERE project_id = $1 AND dataset_epoch = $2) AS payments,
            (SELECT count(*) FROM journal_entries WHERE project_id = $1 AND dataset_epoch = $2) AS entries",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_one(&mut *tx)
    .await?;
    // Every payment-linked account balance must equal what the committed sums imply:
    // held = authorized − captured − voided, pending = captured − refunded,
    // funding = authorized − voided − refunded (signed: funding is debit-normal).
    let account_mismatches: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "WITH implied AS (
             SELECT currency, 'payer_held' AS account,
                    -sum(authorized_minor - captured_minor - voided_minor) AS amount
               FROM payments WHERE project_id = $1 AND dataset_epoch = $2 GROUP BY currency
             UNION ALL
             SELECT currency, 'merchant_pending', -sum(captured_minor - refunded_minor)
               FROM payments WHERE project_id = $1 AND dataset_epoch = $2 GROUP BY currency
             UNION ALL
             SELECT currency, 'payer_funding', sum(authorized_minor - voided_minor - refunded_minor)
               FROM payments WHERE project_id = $1 AND dataset_epoch = $2 GROUP BY currency
         ), journal AS (
             SELECT currency, account_kind AS account, sum(amount_minor) AS amount
               FROM journal_lines WHERE project_id = $1 AND dataset_epoch = $2
                AND account_kind IN ('payer_held', 'merchant_pending', 'payer_funding')
              GROUP BY currency, account_kind
         )
         SELECT coalesce(i.currency, j.currency), coalesce(i.account, j.account),
                coalesce(j.amount, 0)::bigint, coalesce(i.amount, 0)::bigint
           FROM implied i FULL JOIN journal j ON j.currency = i.currency AND j.account = i.account
          WHERE coalesce(j.amount, 0) <> coalesce(i.amount, 0)
          ORDER BY 1, 2",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Report {
        account_mismatches,
        imbalanced_currencies,
        payment_mismatches,
        operation_entry_mismatches: row.try_get("mismatches")?,
        open_operations: row.try_get("open")?,
        payments_checked: row.try_get("payments")?,
        entries_checked: row.try_get("entries")?,
    })
}

/// One line of an account's explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExplainedLine {
    /// Entry allocation order.
    pub seq: i64,
    /// Entry id.
    pub entry_id: Uuid,
    /// Recipe.
    pub recipe: String,
    /// Payment, if any.
    pub payment_id: Option<Uuid>,
    /// Operation, if any.
    pub operation_id: Option<Uuid>,
    /// Signed minor units on this account.
    pub amount_minor: i64,
    /// Signed running total of the account through this line.
    pub running_minor: i64,
}

/// A page of the postings that sum to an account's balance, in posting order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Explanation {
    /// The account's signed balance (every line, not just this page).
    pub balance_minor: i64,
    /// Number of lines in total.
    pub line_count: i64,
    /// This page.
    pub lines: Vec<ExplainedLine>,
    /// Pass as `after_seq` for the next page; `None` at the end.
    pub next_after_seq: Option<i64>,
}

/// Explains `account` in `currency`: the lines with entry seq after `after_seq`, at most
/// `limit`, each with the running total, plus the total balance. Summing every page's lines
/// gives exactly the balance.
pub async fn explain(
    pool: &PgPool,
    scope: ProjectScope,
    currency: &str,
    account: &str,
    after_seq: i64,
    limit: i64,
) -> Result<Explanation, DbError> {
    let limit = limit.clamp(1, 1_000);
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let (balance_minor, line_count): (i64, i64) = sqlx::query_as(
        "SELECT coalesce(sum(amount_minor), 0)::bigint, count(*) FROM journal_lines
          WHERE project_id = $1 AND dataset_epoch = $2 AND currency = $3 AND account_kind = $4",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(currency)
    .bind(account)
    .fetch_one(&mut *tx)
    .await?;
    // The running total before this page, once.
    let before_page: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(l.amount_minor), 0)::bigint FROM journal_lines l
           JOIN journal_entries e ON e.project_id = l.project_id AND e.id = l.entry_id
          WHERE l.project_id = $1 AND l.dataset_epoch = $2 AND l.currency = $3
            AND l.account_kind = $4 AND e.seq <= $5",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(currency)
    .bind(account)
    .bind(after_seq)
    .fetch_one(&mut *tx)
    .await?;
    let rows = sqlx::query(
        "SELECT e.seq, e.id, e.recipe, e.payment_id, e.operation_id, l.amount_minor
           FROM journal_lines l
           JOIN journal_entries e ON e.project_id = l.project_id AND e.id = l.entry_id
          WHERE l.project_id = $1 AND l.dataset_epoch = $2 AND l.currency = $3 AND l.account_kind = $4
            AND e.seq > $5
          ORDER BY e.seq, l.line_no
          LIMIT $6",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(currency)
    .bind(account)
    .bind(after_seq)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    // Every recipe posts to an account at most once per entry, so paging by entry
    // sequence never splits an entry's lines on one account.
    let mut running = before_page;
    let mut lines = Vec::with_capacity(rows.len());
    for row in &rows {
        let amount: i64 = row.try_get("amount_minor")?;
        let total = running
            .checked_add(amount)
            .ok_or_else(|| DbError::Constraint("running total overflow".to_owned()))?;
        running = total;
        lines.push(ExplainedLine {
            seq: row.try_get("seq")?,
            entry_id: row.try_get("id")?,
            recipe: row.try_get("recipe")?,
            payment_id: row.try_get("payment_id")?,
            operation_id: row.try_get("operation_id")?,
            amount_minor: amount,
            running_minor: total,
        });
    }
    let next_after_seq = if lines.len() == usize::try_from(limit).unwrap_or(0) {
        lines.last().map(|l| l.seq)
    } else {
        None
    };
    Ok(Explanation {
        balance_minor,
        line_count,
        lines,
        next_after_seq,
    })
}
