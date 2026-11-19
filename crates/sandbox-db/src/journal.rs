//! Appending to and reading the journal. There is deliberately no update or delete function:
//! the schema would refuse it anyway (PSJ01), and a correction is a reversing entry.

use sandbox_domain::{AccountKind, Currency, JournalEntry, LEDGER_SEMANTICS_VERSION};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor, PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DbError, ProjectScope};

/// What an entry is about, beyond its lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryRefs {
    /// The operation whose success it records.
    pub operation_id: Option<Uuid>,
    /// The payment it belongs to.
    pub payment_id: Option<Uuid>,
    /// For a reversal: the entry it mirrors.
    pub reverses: Option<Uuid>,
}

/// A committed (or about-to-commit) entry's database identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostedEntry {
    /// The derived entry id.
    pub id: Uuid,
    /// Allocation order.
    pub seq: i64,
}

/// Appends `entry` in one statement (entry row and all lines together, as the balance
/// trigger requires), inside the caller's transaction.
pub async fn post(
    conn: &mut PgConnection,
    scope: ProjectScope,
    id: Uuid,
    entry: &JournalEntry,
    refs: EntryRefs,
) -> Result<PostedEntry, DbError> {
    let lines = entry.lines();
    let line_nos: Vec<i16> = (1..=lines.len())
        .map(|n| i16::try_from(n).unwrap_or(i16::MAX))
        .collect();
    let accounts: Vec<&str> = lines.iter().map(|l| l.account.code()).collect();
    let amounts: Vec<i64> = lines
        .iter()
        .map(sandbox_domain::Line::signed_minor)
        .collect();
    let row = sqlx::query(
        "WITH entry AS (
             INSERT INTO journal_entries
                 (project_id, id, dataset_epoch, currency, recipe, semantics_version,
                  operation_id, payment_id, reverses_entry_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             RETURNING project_id, id, seq, currency, dataset_epoch
         ), lines AS (
             INSERT INTO journal_lines
                 (project_id, entry_id, line_no, currency, dataset_epoch, account_kind, amount_minor)
             SELECT entry.project_id, entry.id, l.line_no, entry.currency, entry.dataset_epoch,
                    l.account_kind, l.amount_minor
               FROM entry, unnest($10::smallint[], $11::text[], $12::bigint[])
                    AS l(line_no, account_kind, amount_minor)
         ), balances AS (
             -- The derived balance moves with the lines, in the same statement (D-096):
             -- a committed line is always inside its account's balance.
             INSERT INTO account_balances
                 (project_id, dataset_epoch, currency, account_kind, balance_minor, line_count)
             SELECT entry.project_id, entry.dataset_epoch, entry.currency, d.account_kind,
                    d.delta, d.n
               FROM entry,
                    (SELECT account_kind, sum(amount_minor) AS delta, count(*) AS n
                       FROM unnest($10::smallint[], $11::text[], $12::bigint[])
                            AS l(line_no, account_kind, amount_minor)
                      GROUP BY account_kind
                      -- A fixed row order across every posting: concurrent upserts to
                      -- the same accounts then take the row locks in the same order,
                      -- never deadlock on each other.
                      ORDER BY account_kind) d
             ON CONFLICT (project_id, dataset_epoch, currency, account_kind)
             DO UPDATE SET balance_minor = account_balances.balance_minor + EXCLUDED.balance_minor,
                           line_count = account_balances.line_count + EXCLUDED.line_count
         )
         SELECT seq FROM entry",
    )
    .bind(scope.project_id)
    .bind(id)
    .bind(scope.epoch)
    .bind(entry.currency().code())
    .bind(entry.recipe().code())
    .bind(LEDGER_SEMANTICS_VERSION)
    .bind(refs.operation_id)
    .bind(refs.payment_id)
    .bind(refs.reverses)
    .bind(&line_nos)
    .bind(&accounts)
    .bind(&amounts)
    .fetch_one(&mut *conn)
    .await?;
    Ok(PostedEntry {
        id,
        seq: row.try_get("seq")?,
    })
}

/// One account's balance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Balance {
    /// Currency.
    pub currency: Currency,
    /// Account.
    pub account: AccountKind,
    /// Signed sum of its lines (debit positive).
    pub signed_minor: i64,
}

/// Every account balance of the project's current epoch, read from the derived balances
/// kept with each posting (D-096); reconciliation re-derives them from the journal and
/// reports any drift, so the journal stays the only authority.
pub async fn balances(pool: &PgPool, scope: ProjectScope) -> Result<Vec<Balance>, DbError> {
    let rows = sqlx::query(
        "SELECT currency, account_kind, balance_minor AS total
           FROM account_balances
          WHERE project_id = $1 AND dataset_epoch = $2
          ORDER BY currency, account_kind",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let currency: String = row.try_get("currency")?;
            let account: String = row.try_get("account_kind")?;
            Ok(Balance {
                currency: Currency::from_code(&currency)
                    .map_err(|_| DbError::Constraint(format!("currency {currency}")))?,
                account: AccountKind::from_code(&account)
                    .map_err(|_| DbError::Constraint(format!("account {account}")))?,
                signed_minor: row.try_get("total")?,
            })
        })
        .collect()
}

/// A stored entry with its lines, for inspection and explain-balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredEntry {
    /// Entry id.
    pub id: Uuid,
    /// Allocation order.
    pub seq: i64,
    /// Recipe code.
    pub recipe: String,
    /// Currency code.
    pub currency: String,
    /// Ledger semantics version it was posted under.
    pub semantics_version: i32,
    /// Operation, if any.
    pub operation_id: Option<Uuid>,
    /// Payment, if any.
    pub payment_id: Option<Uuid>,
    /// Database clock at posting.
    #[serde(with = "time::serde::rfc3339")]
    pub posted_at: OffsetDateTime,
    /// `(account, signed minor units)` per line, in line order.
    pub lines: Vec<(String, i64)>,
}

/// Every entry of a payment, oldest first.
pub async fn entries_for_payment<'e>(
    db: impl PgExecutor<'e>,
    scope: ProjectScope,
    payment_id: Uuid,
) -> Result<Vec<StoredEntry>, DbError> {
    let rows = sqlx::query(
        "SELECT e.id, e.seq, e.recipe, e.currency, e.semantics_version, e.operation_id,
                e.payment_id, e.posted_at,
                array_agg(l.account_kind ORDER BY l.line_no) AS accounts,
                array_agg(l.amount_minor ORDER BY l.line_no) AS amounts
           FROM journal_entries e
           JOIN journal_lines l ON l.project_id = e.project_id AND l.entry_id = e.id
          WHERE e.project_id = $1 AND e.dataset_epoch = $2 AND e.payment_id = $3
          GROUP BY e.project_id, e.id
          ORDER BY e.seq",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(payment_id)
    .fetch_all(db)
    .await?;
    rows.iter().map(stored_entry).collect()
}

pub(crate) fn stored_entry(row: &sqlx::postgres::PgRow) -> Result<StoredEntry, DbError> {
    let accounts: Vec<String> = row.try_get("accounts")?;
    let amounts: Vec<i64> = row.try_get("amounts")?;
    Ok(StoredEntry {
        id: row.try_get("id")?,
        seq: row.try_get("seq")?,
        recipe: row.try_get("recipe")?,
        currency: row.try_get("currency")?,
        semantics_version: row.try_get("semantics_version")?,
        operation_id: row.try_get("operation_id")?,
        payment_id: row.try_get("payment_id")?,
        posted_at: row.try_get("posted_at")?,
        lines: accounts.into_iter().zip(amounts).collect(),
    })
}

/// Per-currency totals of every line of the project's epoch: each must be zero.
pub async fn imbalance_by_currency(
    pool: &PgPool,
    scope: ProjectScope,
) -> Result<Vec<(String, i64)>, DbError> {
    let rows = sqlx::query(
        "SELECT currency, sum(amount_minor)::bigint AS total
           FROM journal_lines
          WHERE project_id = $1 AND dataset_epoch = $2
          GROUP BY currency
         HAVING sum(amount_minor) <> 0",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("currency")?, row.try_get("total")?)))
        .collect()
}
