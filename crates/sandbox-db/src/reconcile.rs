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
    tx.commit().await?;
    Ok(Report {
        imbalanced_currencies,
        payment_mismatches,
        operation_entry_mismatches: row.try_get("mismatches")?,
        open_operations: row.try_get("open")?,
        payments_checked: row.try_get("payments")?,
        entries_checked: row.try_get("entries")?,
    })
}
