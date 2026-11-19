//! Reconciliation: prove the derived records agree with the journal (D-003, D-028).
//!
//! - every currency's journal lines sum to zero;
//! - every payment's committed sums equal the totals of its journal entries;
//! - every succeeded operation has exactly one entry, and every entry's operation
//!   succeeded;
//! - every derived account balance (D-096) equals the journal's sums: the journal stays
//!   the only authority and a drifted derivation is reported, never trusted.
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
    /// Accounts whose derived balance (D-096) disagrees with the journal's sums:
    /// `(currency, account, journal, derived)`. The journal side is the truth a drift
    /// is measured against.
    pub balance_mismatches: Vec<(String, String, i64, i64)>,
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
            && self.balance_mismatches.is_empty()
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
    let payment_mismatches = payment_mismatches(&mut tx, scope).await?;
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
    let balance_mismatches = balance_mismatches(&mut tx, scope).await?;
    tx.commit().await?;
    Ok(Report {
        account_mismatches,
        balance_mismatches,
        imbalanced_currencies,
        payment_mismatches,
        operation_entry_mismatches: row.try_get("mismatches")?,
        open_operations: row.try_get("open")?,
        payments_checked: row.try_get("payments")?,
        entries_checked: row.try_get("entries")?,
    })
}

/// Index-driven per payment (payments, then `journal_entries_by_payment`, then the lines'
/// primary key), so the plan does not depend on fresh statistics: a just-written project
/// otherwise invites a nested loop over sequential scans.
async fn payment_mismatches(
    tx: &mut sqlx::PgConnection,
    scope: ProjectScope,
) -> Result<Vec<Uuid>, DbError> {
    Ok(sqlx::query_scalar(
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
    .await?)
}

/// The derived balances re-derived from the journal (D-096): every row of each side must
/// appear on the other with the same sum — a drifted, missing or phantom balance row is a
/// mismatch, not a thing to be repaired silently.
async fn balance_mismatches(
    tx: &mut sqlx::PgConnection,
    scope: ProjectScope,
) -> Result<Vec<(String, String, i64, i64)>, DbError> {
    Ok(sqlx::query_as(
        "WITH journal AS (
             SELECT currency, account_kind, sum(amount_minor) AS amount, count(*) AS lines
               FROM journal_lines
              WHERE project_id = $1 AND dataset_epoch = $2
              GROUP BY currency, account_kind
         ), derived AS (
             -- Scoped in a subquery, not the ON clause: a FULL JOIN emits every row of
             -- both sides, so a scope in ON would only change the pairing, and rows of
             -- other epochs would still appear as phantom mismatches. The stripes are
             -- summed away: the balance is per account, not per stripe.
             SELECT currency, account_kind,
                    sum(balance_minor)::bigint AS balance_minor, sum(line_count)::bigint AS line_count
               FROM account_balances
              WHERE project_id = $1 AND dataset_epoch = $2
              GROUP BY currency, account_kind
         )
         SELECT coalesce(j.currency, d.currency), coalesce(j.account_kind, d.account_kind),
                coalesce(j.amount, 0)::bigint, coalesce(d.balance_minor, 0)::bigint
           FROM journal j FULL JOIN derived d
             ON d.currency = j.currency AND d.account_kind = j.account_kind
          WHERE coalesce(j.amount, 0) <> coalesce(d.balance_minor, 0)
             OR coalesce(j.lines, 0) <> coalesce(d.line_count, 0)
          ORDER BY 1, 2",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_all(&mut *tx)
    .await?)
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

/// The database's restore generation (D-050, review finding 16): its cluster's system
/// identifier, the cluster's timeline and the database's OID. A logical restore (`pg_dump`
/// into a fresh database, D-026) creates a new database, so a new OID; a physical restore
/// that is promoted starts a new timeline; a restore into another cluster has another
/// system identifier. Sequence numbers handed out after a restore can repeat ones from the
/// lost history, so a cursor is valid only in the generation it was taken in.
pub async fn generation(pool: &PgPool) -> Result<String, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT concat_ws('.', s.system_identifier, c.timeline_id, d.oid)
           FROM pg_control_system() s, pg_control_checkpoint() c, pg_database d
          WHERE d.datname = current_database()",
    )
    .fetch_one(pool)
    .await?)
}

/// Where the next page of an explanation starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExplainCursor {
    /// The restore generation the explanation began in (D-050); any other is refused.
    pub generation: String,
    /// The first page's snapshot (`xmin:xmax:xip,…`): only entries of transactions it saw
    /// as committed count.
    pub as_of: String,
    /// Entries after this allocation sequence.
    pub after_seq: i64,
    /// The running total through the previous page.
    pub running: i64,
}

/// A page of the postings that sum to an account's balance as of one snapshot. An entry
/// restored from another cluster (its row's `xmin` is not its `created_xact`, D-055) counts
/// as committed before every snapshot: its stored transaction id means nothing here (review
/// N2). (Review
/// finding 5; D-042): the first page's snapshot. The entries its transactions committed
/// are a fixed set — a transaction in progress at that moment stays invisible to it even
/// after it commits — so paging by sequence over it is stable and every page reports the
/// same balance, which is the live balance at that moment. Later commits appear in the next
/// explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Explanation {
    /// The database's restore generation (D-050).
    pub generation: String,
    /// The snapshot this explanation is of.
    pub as_of: String,
    /// The account's signed balance at the horizon (every line, not just this page).
    pub balance_minor: i64,
    /// Number of lines at the horizon.
    pub line_count: i64,
    /// This page.
    pub lines: Vec<ExplainedLine>,
    /// The cursor of the next page; `None` at the end.
    pub next: Option<ExplainCursor>,
}

/// Explains `account` in `currency`. Without a cursor it starts at the current horizon.
pub async fn explain(
    pool: &PgPool,
    scope: ProjectScope,
    currency: &str,
    account: &str,
    cursor: Option<ExplainCursor>,
    limit: i64,
) -> Result<Explanation, DbError> {
    let limit = limit.clamp(1, 1_000);
    let generation = generation(pool).await?;
    let (as_of, after_seq, running) = match cursor {
        Some(c) if c.generation != generation => return Err(DbError::StaleCursor),
        Some(c) => (c.as_of, c.after_seq, c.running),
        None => (
            sqlx::query_scalar("SELECT pg_current_snapshot()::text")
                .fetch_one(pool)
                .await?,
            0,
            0,
        ),
    };
    // The horizon balance comes from the derived balance minus the lines invisible to the
    // snapshot — usually a small recent tail — in one statement, so the two reads share a
    // moment (D-096). `line_count` is corrected the same way; an account with no lines at
    // all reports zero, as the plain sum did.
    let (balance_minor, line_count): (i64, i64) = sqlx::query_as(
        "SELECT coalesce(b.balance_minor, 0)::bigint - coalesce(inv.amount, 0)::bigint AS balance,
                coalesce(b.line_count, 0)::bigint - coalesce(inv.lines, 0)::bigint AS lines
           FROM (SELECT 1) one
           LEFT JOIN LATERAL (
             SELECT sum(balance_minor)::bigint AS balance_minor, sum(line_count)::bigint AS line_count
               FROM account_balances
              WHERE project_id = $1 AND dataset_epoch = $2 AND currency = $3 AND account_kind = $4
           ) b ON true
           LEFT JOIN LATERAL (
             SELECT sum(l.amount_minor)::bigint AS amount, count(*)::bigint AS lines
               FROM journal_lines l
               JOIN journal_entries e ON e.project_id = l.project_id AND e.id = l.entry_id
              WHERE l.project_id = $1 AND l.dataset_epoch = $2 AND l.currency = $3 AND l.account_kind = $4
                AND e.created_xact >= pg_snapshot_xmin($5::pg_snapshot)
                AND NOT (e.xmin <> e.created_xact::xid
                         OR pg_visible_in_snapshot(e.created_xact, $5::pg_snapshot))
           ) inv ON true",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(currency)
    .bind(account)
    .bind(&as_of)
    .fetch_one(pool)
    .await?;
    let rows = sqlx::query(
        "SELECT e.seq, e.id, e.recipe, e.payment_id, e.operation_id, l.amount_minor
           FROM journal_lines l
           JOIN journal_entries e ON e.project_id = l.project_id AND e.id = l.entry_id
          WHERE l.project_id = $1 AND l.dataset_epoch = $2 AND l.currency = $3 AND l.account_kind = $4
            AND (e.xmin <> e.created_xact::xid OR pg_visible_in_snapshot(e.created_xact, $5::pg_snapshot)) AND e.seq > $6
          ORDER BY e.seq, l.line_no
          LIMIT $7",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(currency)
    .bind(account)
    .bind(&as_of)
    .bind(after_seq)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    // Every recipe posts to an account at most once per entry, so paging by entry sequence
    // never splits an entry's lines on one account.
    let mut total = running;
    let mut lines = Vec::with_capacity(rows.len());
    for row in &rows {
        let amount: i64 = row.try_get("amount_minor")?;
        total = total
            .checked_add(amount)
            .ok_or_else(|| DbError::Constraint("running total overflow".to_owned()))?;
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
    let next = (lines.len() == usize::try_from(limit).unwrap_or(0))
        .then(|| {
            lines.last().map(|l| ExplainCursor {
                generation: generation.clone(),
                as_of: as_of.clone(),
                after_seq: l.seq,
                running: total,
            })
        })
        .flatten();
    Ok(Explanation {
        generation,
        as_of,
        balance_minor,
        line_count,
        lines,
        next,
    })
}
