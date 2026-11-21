//! Payments and operations: the transactions on either side of a network call.
//!
//! 1. **Begin** commits the operation's identity (and, for captures, voids and refunds, its
//!    capacity reservation under the payment's row lock) *before* the network is called.
//! 2. The caller calls the simulated network with no transaction open.
//! 3. **Apply** records the answer exactly once: under the payment and operation locks it
//!    moves the reservation into the committed sums (or releases it), posts the balanced
//!    journal entry, and resolves the operation. A second apply finds it resolved.
//!
//! Lock order is always payment, then operation, so concurrent requests cannot deadlock.

use sandbox_domain::identity::entry_id;
use sandbox_domain::payment::Reservation;
use sandbox_domain::{
    Amount, CapacityError, Currency, NetworkOutcome, OperationKind, OperationState, PaymentStatus,
    PaymentSums,
};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor, PgPool, Row, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::journal::{self, EntryRefs};
use crate::{DbError, ProjectScope};

// Column lists as macros, so every statement is one `&'static str` built by `concat!`.
macro_rules! payment_columns {
    () => {
        "id, authorize_operation_id, currency, amount_minor, token, description, status,
         authorized_minor, captured_minor, capture_reserved_minor, voided_minor, void_reserved_minor,
         refunded_minor, refund_reserved_minor, decline_code, created_at, updated_at, event_sequence"
    };
}

macro_rules! operation_columns {
    () => {
        "id, payment_id, kind, idempotency_key, fingerprint, amount_minor, state,
         decline_code, network_ref, attempts, created_at, resolved_at"
    };
}

/// A payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Payment {
    /// Id (derived from its authorize operation).
    pub id: Uuid,
    /// The authorize operation.
    pub authorize_operation_id: Uuid,
    /// Currency.
    pub currency: Currency,
    /// Requested authorization amount.
    pub amount_minor: i64,
    /// Scenario token.
    pub token: String,
    /// Merchant description.
    pub description: Option<String>,
    /// Derived status code.
    pub status: String,
    /// Committed sums and reservations.
    pub sums: PaymentSums,
    /// Why the authorization was declined.
    pub decline_code: Option<String>,
    /// Created (database clock).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last changed (database clock).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Sequence of the payment's latest event.
    pub event_sequence: i64,
}

/// An operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Operation {
    /// Identity (UUIDv5 of project, epoch, kind and key).
    pub id: Uuid,
    /// Its payment.
    pub payment_id: Uuid,
    /// Kind.
    pub kind: OperationKind,
    /// The client's key.
    pub idempotency_key: String,
    /// SHA-256 of the canonical request.
    #[serde(skip)]
    pub fingerprint: Vec<u8>,
    /// Minor units.
    pub amount_minor: i64,
    /// State.
    pub state: OperationState,
    /// Decline reason.
    pub decline_code: Option<String>,
    /// The network's reference.
    pub network_ref: Option<String>,
    /// Network calls attempted.
    pub attempts: i32,
    /// Created (database clock).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Resolved (database clock).
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
}

/// The result of beginning an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Begin {
    /// Newly recorded; the caller must call the network.
    Started(Operation, Payment),
    /// The same key and request were seen before; the caller replays (or re-drives an open
    /// operation under the same identity).
    Existing(Operation, Payment),
}

/// Why an operation could not begin.
#[derive(Debug, thiserror::Error)]
pub enum BeginError {
    /// The key was used with a different request (D-016).
    #[error("idempotency key reused with a different request")]
    KeyConflict,
    /// No such payment in this project.
    #[error("payment not found")]
    PaymentNotFound,
    /// A capacity refusal; nothing was recorded.
    #[error(transparent)]
    Capacity(#[from] CapacityError),
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for BeginError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::from(e))
    }
}

/// A new authorization.
#[derive(Debug, Clone)]
pub struct NewAuthorize<'a> {
    /// Derived operation id.
    pub operation_id: Uuid,
    /// Derived payment id.
    pub payment_id: Uuid,
    /// The client's key.
    pub idempotency_key: &'a str,
    /// Request fingerprint.
    pub fingerprint: [u8; 32],
    /// Amount.
    pub amount: Amount,
    /// Currency.
    pub currency: Currency,
    /// Scenario token.
    pub token: &'a str,
    /// Description.
    pub description: Option<&'a str>,
}

/// The project's current epoch under a key-share lock: reset's `FOR UPDATE` conflicts
/// with it, so this transaction either precedes the epoch move or sees it — never
/// writes beside it. A cached identity's epoch can lag a cross-process reset by the
/// notification latency (S1-1); without the check, `scope.epoch` bound writes into the
/// archived epoch for up to the key cache's TTL. Concurrent writers hold only the
/// shared lock — they do not serialize on each other, only on the rare reset.
async fn ensure_epoch(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: ProjectScope,
) -> Result<(), DbError> {
    let current: Option<i32> =
        sqlx::query_scalar("SELECT dataset_epoch FROM projects WHERE id = $1 FOR KEY SHARE")
            .bind(scope.project_id)
            .fetch_optional(&mut **tx)
            .await?;
    match current {
        Some(epoch) if epoch == scope.epoch => Ok(()),
        Some(_) => Err(DbError::EpochMoved),
        None => Err(DbError::NotFound),
    }
}

/// Records a new payment and its authorize operation (`pending`), or finds the existing
/// one for the same identity.
pub async fn begin_authorize(
    pool: &PgPool,
    scope: ProjectScope,
    new: &NewAuthorize<'_>,
) -> Result<Begin, BeginError> {
    let mut tx = pool.begin().await?;
    ensure_epoch(&mut tx, scope).await?;
    let inserted = sqlx::query(
        "INSERT INTO payments (project_id, id, dataset_epoch, authorize_operation_id, currency,
                               amount_minor, token, description, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'authorizing')
         ON CONFLICT (project_id, id) DO NOTHING",
    )
    .bind(scope.project_id)
    .bind(new.payment_id)
    .bind(scope.epoch)
    .bind(new.operation_id)
    .bind(new.currency.code())
    .bind(new.amount.minor())
    .bind(new.token)
    .bind(new.description)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        drop(tx);
        let operation = get_operation(pool, scope, new.operation_id)
            .await?
            .ok_or(BeginError::Db(DbError::NotFound))?;
        if operation.fingerprint != new.fingerprint {
            return Err(BeginError::KeyConflict);
        }
        let payment = get_payment(pool, scope, new.payment_id)
            .await?
            .ok_or(BeginError::Db(DbError::NotFound))?;
        return Ok(Begin::Existing(operation, payment));
    }
    let operation = insert_operation(
        &mut tx,
        scope,
        new.operation_id,
        new.payment_id,
        OperationKind::Authorize,
        new.idempotency_key,
        &new.fingerprint,
        new.amount.minor(),
    )
    .await?;
    let payment = lock_payment(&mut tx, scope, new.payment_id)
        .await?
        .ok_or(BeginError::PaymentNotFound)?;
    tx.commit().await?;
    Ok(Begin::Started(operation, payment))
}

/// A new capture, void or refund.
#[derive(Debug, Clone)]
pub struct NewChild<'a> {
    /// Derived operation id.
    pub operation_id: Uuid,
    /// The payment it acts on.
    pub payment_id: Uuid,
    /// Capture, void or refund.
    pub kind: OperationKind,
    /// The client's key.
    pub idempotency_key: &'a str,
    /// Request fingerprint.
    pub fingerprint: [u8; 32],
    /// The amount; `None` for a void, which releases exactly the uncaptured remainder.
    pub amount: Option<Amount>,
}

/// Records a capture, void or refund and reserves its capacity, in one transaction under
/// the payment's row lock (D-014). Concurrent requests serialize here; the one that would
/// overrun is refused and records nothing.
pub async fn begin_child(
    pool: &PgPool,
    scope: ProjectScope,
    new: &NewChild<'_>,
) -> Result<Begin, BeginError> {
    let reservation = new
        .kind
        .reservation()
        .ok_or(BeginError::Db(DbError::Constraint(
            "authorize is not a child operation".to_owned(),
        )))?;
    let mut tx = pool.begin().await?;
    ensure_epoch(&mut tx, scope).await?;
    // One round trip: lock the payment and learn whether this identity already exists.
    let row = sqlx::query(concat!(
        "SELECT ",
        payment_columns!(),
        ", (SELECT o.fingerprint FROM operations o
             WHERE o.project_id = payments.project_id AND o.id = $4) AS existing_fingerprint
           FROM payments WHERE project_id = $1 AND dataset_epoch = $2 AND id = $3 FOR UPDATE"
    ))
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(new.payment_id)
    .bind(new.operation_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(BeginError::PaymentNotFound)?;
    let payment = payment_from(&row)?;
    let existing: Option<Vec<u8>> = row.try_get("existing_fingerprint").map_err(DbError::from)?;
    if let Some(fingerprint) = existing {
        if fingerprint != new.fingerprint {
            return Err(BeginError::KeyConflict);
        }
        let existing = fetch_operation(&mut tx, scope, new.operation_id)
            .await?
            .ok_or(BeginError::Db(DbError::NotFound))?;
        return Ok(Begin::Existing(existing, payment));
    }
    let amount = match (reservation, new.amount) {
        (Reservation::Void, _) => payment.sums.void_amount()?,
        (_, Some(amount)) => amount,
        (_, None) => return Err(BeginError::Capacity(CapacityError::NothingToVoid)),
    };
    let sums = payment.sums.reserve(reservation, amount)?;
    let status = PaymentStatus::derive(OperationState::Succeeded, &sums);
    // One round trip: record the operation and its reservation together.
    let row = sqlx::query(concat!(
        "WITH op AS (
             INSERT INTO operations (project_id, id, dataset_epoch, payment_id, kind,
                                     idempotency_key, fingerprint, amount_minor, state)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending')
         )
         UPDATE payments
            SET capture_reserved_minor = $9, void_reserved_minor = $10,
                refund_reserved_minor = $11, status = $12, updated_at = now()
          WHERE project_id = $1 AND id = $4
          RETURNING ",
        payment_columns!(),
        ""
    ))
    .bind(scope.project_id)
    .bind(new.operation_id)
    .bind(scope.epoch)
    .bind(new.payment_id)
    .bind(new.kind.code())
    .bind(new.idempotency_key)
    .bind(new.fingerprint.as_slice())
    .bind(amount.minor())
    .bind(sums.capture_reserved)
    .bind(sums.void_reserved)
    .bind(sums.refund_reserved)
    .bind(status.code())
    .fetch_one(&mut *tx)
    .await?;
    let payment = payment_from(&row)?;
    tx.commit().await?;
    let operation = Operation {
        id: new.operation_id,
        payment_id: new.payment_id,
        kind: new.kind,
        idempotency_key: new.idempotency_key.to_owned(),
        fingerprint: new.fingerprint.to_vec(),
        amount_minor: amount.minor(),
        state: OperationState::Pending,
        decline_code: None,
        network_ref: None,
        attempts: 0,
        created_at: payment.updated_at,
        resolved_at: None,
    };
    Ok(Begin::Started(operation, payment))
}

/// What the network answered (or failed to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkResult {
    /// Approved, declined, unknown, or (fenced readback) not executed.
    pub outcome: NetworkOutcome,
    /// Decline reason.
    pub decline_code: Option<String>,
    /// The network's reference.
    pub network_ref: Option<String>,
}

/// The result of applying an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Applied now.
    Applied(Operation, Payment),
    /// Already resolved earlier; nothing changed.
    AlreadyResolved(Operation, Payment),
}

impl Applied {
    /// The operation and payment, either way.
    pub fn into_parts(self) -> (Operation, Payment) {
        match self {
            Self::Applied(o, p) | Self::AlreadyResolved(o, p) => (o, p),
        }
    }
}

/// Applies the network's answer to an open operation, exactly once.
pub async fn apply(
    pool: &PgPool,
    scope: ProjectScope,
    payment_id: Uuid,
    operation_id: Uuid,
    result: &NetworkResult,
) -> Result<Applied, DbError> {
    let mut tx = pool.begin().await?;
    let payment = lock_payment(&mut tx, scope, payment_id)
        .await?
        .ok_or(DbError::NotFound)?;
    let operation = lock_operation(&mut tx, scope, operation_id)
        .await?
        .ok_or(DbError::NotFound)?;
    if operation.payment_id != payment_id {
        return Err(DbError::NotFound);
    }
    let Ok(next_state) = operation.state.apply(result.outcome) else {
        return Ok(Applied::AlreadyResolved(operation, payment));
    };
    let amount = Amount::new(operation.amount_minor)
        .map_err(|_| DbError::Constraint("stored operation amount".to_owned()))?;

    let succeeded = next_state == OperationState::Succeeded;
    let resolved = next_state.is_terminal();
    let (sums, decline_code) = next_sums(&payment, &operation, next_state, amount, result)?;
    if succeeded {
        let recipe = operation.kind.recipe();
        let entry = recipe
            .post(payment.currency, amount)
            .map_err(|e| DbError::Constraint(format!("posting recipe: {e}")))?;
        journal::post(
            &mut tx,
            scope,
            entry_id(operation.id, recipe),
            &entry,
            EntryRefs {
                operation_id: Some(operation.id),
                payment_id: Some(payment.id),
                reverses: None,
            },
        )
        .await?;
    }
    let authorization_state = if operation.kind == OperationKind::Authorize {
        next_state
    } else {
        OperationState::Succeeded
    };
    let status = PaymentStatus::derive(authorization_state, &sums);
    // One round trip: resolve the operation and update the payment together.
    let row = sqlx::query(concat!(
        "WITH op AS (
             UPDATE operations
                SET state = $12,
                    decline_code = coalesce($13, decline_code),
                    network_ref = coalesce($14, network_ref),
                    attempts = attempts + 1,
                    resolved_at = CASE WHEN $15 THEN now() ELSE NULL END
              WHERE project_id = $1 AND id = $3
         )
         UPDATE payments
            SET authorized_minor = $4, captured_minor = $5, capture_reserved_minor = $6,
                voided_minor = $7, void_reserved_minor = $8, refunded_minor = $9,
                refund_reserved_minor = $10, status = $11, decline_code = $16, updated_at = now(),
                event_sequence = event_sequence + CASE WHEN $15 THEN 1 ELSE 0 END
          WHERE project_id = $1 AND id = $2
          RETURNING ",
        payment_columns!(),
        ""
    ))
    .bind(scope.project_id)
    .bind(payment.id)
    .bind(operation.id)
    .bind(sums.authorized)
    .bind(sums.captured)
    .bind(sums.capture_reserved)
    .bind(sums.voided)
    .bind(sums.void_reserved)
    .bind(sums.refunded)
    .bind(sums.refund_reserved)
    .bind(status.code())
    .bind(next_state.code())
    .bind(result.decline_code.as_deref())
    .bind(result.network_ref.as_deref())
    .bind(resolved)
    .bind(decline_code.as_deref())
    .fetch_one(&mut *tx)
    .await?;
    let payment = payment_from(&row)?;
    if resolved {
        emit_event(&mut tx, scope, &payment, &operation, next_state).await?;
    }
    tx.commit().await?;
    let operation = Operation {
        state: next_state,
        decline_code: result.decline_code.clone().or(operation.decline_code),
        network_ref: result.network_ref.clone().or(operation.network_ref),
        attempts: operation.attempts.saturating_add(1),
        resolved_at: resolved.then_some(payment.updated_at),
        ..operation
    };
    Ok(Applied::Applied(operation, payment))
}

/// Commits the resolution's event and one delivery per active endpoint, in the caller's
/// transaction (the outbox, D-006). The event id is derived from the operation, so a
/// re-applied resolution (after a restore) re-creates the same event id and consumers
/// deduplicate it.
async fn emit_event(
    tx: &mut PgConnection,
    scope: ProjectScope,
    payment: &Payment,
    operation: &Operation,
    state: OperationState,
) -> Result<(), DbError> {
    let event_id = Uuid::new_v5(
        &sandbox_domain::identity::NAMESPACE,
        format!("event/v1/{}", operation.id).as_bytes(),
    );
    sqlx::query(
        "WITH e AS (
             INSERT INTO events (project_id, id, dataset_epoch, payment_id, operation_id, kind, sequence)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING project_id, id
         )
         INSERT INTO deliveries (project_id, event_id, endpoint_id)
         SELECT e.project_id, e.id, w.id
           FROM e JOIN webhook_endpoints w ON w.project_id = e.project_id AND w.disabled_at IS NULL",
    )
    .bind(scope.project_id)
    .bind(event_id)
    .bind(scope.epoch)
    .bind(payment.id)
    .bind(operation.id)
    .bind(format!("{}.{}", operation.kind.code(), state.code()))
    .bind(payment.event_sequence)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// The payment's sums and decline code after `operation` moves to `next_state`.
fn next_sums(
    payment: &Payment,
    operation: &Operation,
    next_state: OperationState,
    amount: Amount,
    result: &NetworkResult,
) -> Result<(PaymentSums, Option<String>), DbError> {
    let mut sums = payment.sums;
    let mut decline_code = payment.decline_code.clone();
    let succeeded = next_state == OperationState::Succeeded;
    match operation.kind.reservation() {
        None => {
            // Authorize: no reservation; a success sets the authorized sum.
            if succeeded {
                sums = PaymentSums::authorized(amount);
            }
            if next_state == OperationState::Declined {
                decline_code.clone_from(&result.decline_code);
            }
        }
        Some(reservation) if next_state.is_terminal() => {
            sums = sums
                .complete(reservation, amount, succeeded)
                .map_err(|e| DbError::Constraint(format!("completing a reservation: {e}")))?;
        }
        Some(_) => {}
    }
    Ok((sums, decline_code))
}

/// Loads a payment.
pub async fn get_payment<'e>(
    db: impl PgExecutor<'e>,
    scope: ProjectScope,
    id: Uuid,
) -> Result<Option<Payment>, DbError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        payment_columns!(),
        " FROM payments WHERE project_id = $1 AND dataset_epoch = $2 AND id = $3"
    ))
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(id)
    .fetch_optional(db)
    .await?;
    row.as_ref().map(payment_from).transpose()
}

/// A payment with its operations and, when asked, its journal entries.
#[derive(Debug, Clone)]
pub struct PaymentSnapshot {
    /// The payment.
    pub payment: Payment,
    /// Its operations, oldest first.
    pub operations: Vec<Operation>,
    /// Its journal entries in posting order, when asked for.
    pub entries: Option<Vec<journal::StoredEntry>>,
}

/// A payment, its operations and (with `entries`) its journal entries, read as one
/// snapshot: a single `REPEATABLE READ, READ ONLY` transaction, so an `apply` that commits
/// between the reads can never show "operation succeeded, payment still authorizing"
/// (v1 review M1, D-076).
pub async fn snapshot(
    pool: &PgPool,
    scope: ProjectScope,
    id: Uuid,
    entries: bool,
) -> Result<Option<PaymentSnapshot>, DbError> {
    let mut tx = pool
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .await?;
    // Returning early drops the transaction, which rolls it back (it changed nothing).
    let Some(payment) = get_payment(&mut *tx, scope, id).await? else {
        return Ok(None);
    };
    let operations = operations_for_payment(&mut *tx, scope, id).await?;
    let entries = if entries {
        Some(journal::entries_for_payment(&mut *tx, scope, id).await?)
    } else {
        None
    };
    tx.commit().await?;
    Ok(Some(PaymentSnapshot {
        payment,
        operations,
        entries,
    }))
}

/// The dataset epoch a payment of `project_id` belongs to, whatever the current one is:
/// how a read tells "archived by a reset" (D-066) from "never existed".
pub async fn payment_epoch(
    pool: &PgPool,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<i32>, DbError> {
    Ok(
        sqlx::query_scalar("SELECT dataset_epoch FROM payments WHERE project_id = $1 AND id = $2")
            .bind(project_id)
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// Loads an operation.
pub async fn get_operation(
    pool: &PgPool,
    scope: ProjectScope,
    id: Uuid,
) -> Result<Option<Operation>, DbError> {
    let mut conn = pool.acquire().await?;
    fetch_operation(&mut conn, scope, id).await
}

/// A payment's operations, oldest first.
pub async fn operations_for_payment<'e>(
    db: impl PgExecutor<'e>,
    scope: ProjectScope,
    payment_id: Uuid,
) -> Result<Vec<Operation>, DbError> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        operation_columns!(),
        " FROM operations
          WHERE project_id = $1 AND dataset_epoch = $2 AND payment_id = $3
          ORDER BY created_at, id LIMIT 1000"
    ))
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(payment_id)
    .fetch_all(db)
    .await?;
    rows.iter().map(operation_from).collect()
}

/// A page of the project's payments, newest first: at most `limit` (1–500) older than the
/// payment `before` (keyset paging on `(created_at, id)`, served by `payments_recent`). An
/// unknown `before` gives an empty page.
pub async fn recent_payments(
    pool: &PgPool,
    scope: ProjectScope,
    before: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Payment>, DbError> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        payment_columns!(),
        " FROM payments
          WHERE project_id = $1 AND dataset_epoch = $2
            AND ($3::uuid IS NULL OR (created_at, id) < (
                SELECT created_at, id FROM payments
                 WHERE project_id = $1 AND dataset_epoch = $2 AND id = $3))
          ORDER BY created_at DESC, id DESC LIMIT $4"
    ))
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(before)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    rows.iter().map(payment_from).collect()
}

#[expect(
    clippy::too_many_arguments,
    reason = "one row's columns, bound in one place"
)]
async fn insert_operation(
    tx: &mut PgConnection,
    scope: ProjectScope,
    id: Uuid,
    payment_id: Uuid,
    kind: OperationKind,
    key: &str,
    fingerprint: &[u8; 32],
    amount_minor: i64,
) -> Result<Operation, BeginError> {
    let row = sqlx::query(concat!(
        "INSERT INTO operations (project_id, id, dataset_epoch, payment_id, kind, idempotency_key,
                                 fingerprint, amount_minor, state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending')
         RETURNING ",
        operation_columns!(),
        ""
    ))
    .bind(scope.project_id)
    .bind(id)
    .bind(scope.epoch)
    .bind(payment_id)
    .bind(kind.code())
    .bind(key)
    .bind(fingerprint.as_slice())
    .bind(amount_minor)
    .fetch_one(&mut *tx)
    .await?;
    Ok(operation_from(&row)?)
}

async fn lock_payment(
    tx: &mut PgConnection,
    scope: ProjectScope,
    id: Uuid,
) -> Result<Option<Payment>, DbError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        payment_columns!(),
        " FROM payments
          WHERE project_id = $1 AND dataset_epoch = $2 AND id = $3 FOR UPDATE"
    ))
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    row.as_ref().map(payment_from).transpose()
}

async fn lock_operation(
    tx: &mut PgConnection,
    scope: ProjectScope,
    id: Uuid,
) -> Result<Option<Operation>, DbError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        operation_columns!(),
        " FROM operations WHERE project_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(scope.project_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    row.as_ref().map(operation_from).transpose()
}

async fn fetch_operation(
    conn: &mut PgConnection,
    scope: ProjectScope,
    id: Uuid,
) -> Result<Option<Operation>, DbError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        operation_columns!(),
        " FROM operations WHERE project_id = $1 AND dataset_epoch = $2 AND id = $3"
    ))
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    row.as_ref().map(operation_from).transpose()
}

fn payment_from(row: &PgRow) -> Result<Payment, DbError> {
    let currency: String = row.try_get("currency")?;
    Ok(Payment {
        id: row.try_get("id")?,
        authorize_operation_id: row.try_get("authorize_operation_id")?,
        currency: Currency::from_code(&currency)
            .map_err(|_| DbError::Constraint(format!("currency {currency}")))?,
        amount_minor: row.try_get("amount_minor")?,
        token: row.try_get("token")?,
        description: row.try_get("description")?,
        status: row.try_get("status")?,
        sums: PaymentSums {
            authorized: row.try_get("authorized_minor")?,
            captured: row.try_get("captured_minor")?,
            capture_reserved: row.try_get("capture_reserved_minor")?,
            voided: row.try_get("voided_minor")?,
            void_reserved: row.try_get("void_reserved_minor")?,
            refunded: row.try_get("refunded_minor")?,
            refund_reserved: row.try_get("refund_reserved_minor")?,
        },
        decline_code: row.try_get("decline_code")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        event_sequence: row.try_get("event_sequence")?,
    })
}

fn operation_from(row: &PgRow) -> Result<Operation, DbError> {
    let kind: String = row.try_get("kind")?;
    let state: String = row.try_get("state")?;
    Ok(Operation {
        id: row.try_get("id")?,
        payment_id: row.try_get("payment_id")?,
        kind: OperationKind::from_code(&kind)
            .ok_or_else(|| DbError::Constraint(format!("kind {kind}")))?,
        idempotency_key: row.try_get("idempotency_key")?,
        fingerprint: row.try_get("fingerprint")?,
        amount_minor: row.try_get("amount_minor")?,
        state: OperationState::from_code(&state)
            .ok_or_else(|| DbError::Constraint(format!("state {state}")))?,
        decline_code: row.try_get("decline_code")?,
        network_ref: row.try_get("network_ref")?,
        attempts: row.try_get("attempts")?,
        created_at: row.try_get("created_at")?,
        resolved_at: row.try_get("resolved_at")?,
    })
}

/// An open (pending or uncertain) operation, as the reconciler sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenOperation {
    /// Its project and epoch.
    pub scope: ProjectScope,
    /// The operation.
    pub id: Uuid,
    /// Its payment.
    pub payment_id: Uuid,
}

/// Open operations due for a check, at most `limit`, **taking the projects in turn**
/// (review finding 11): every project's most overdue operation first, then every project's
/// second, and so on, at most `per_project` of any one project. So one project's backlog
/// cannot hold another's recovery back. Only operations older than `grace` are taken
/// (a younger one may still have its request in flight), and only those whose
/// `next_check_at` has come (a failed check backs off, see [`postpone_check`]).
pub async fn open_operations(
    pool: &PgPool,
    grace: std::time::Duration,
    limit: i64,
    per_project: i64,
) -> Result<Vec<OpenOperation>, DbError> {
    let grace_ms = i64::try_from(grace.as_millis()).unwrap_or(i64::MAX);
    let rows = sqlx::query(
        "SELECT o.project_id, o.dataset_epoch, o.id, o.payment_id
           FROM (SELECT DISTINCT project_id FROM operations
                  WHERE state IN ('pending', 'uncertain')) p
           CROSS JOIN LATERAL (
               SELECT q.project_id, q.dataset_epoch, q.id, q.payment_id, q.created_at,
                      row_number() OVER (ORDER BY q.next_check_at, q.created_at) AS turn
                 FROM operations q
                WHERE q.project_id = p.project_id AND q.state IN ('pending', 'uncertain')
                  AND q.next_check_at <= now()
                  AND q.created_at < now() - make_interval(secs => $1::float8 / 1000.0)
                ORDER BY q.next_check_at, q.created_at
                LIMIT $3
           ) o
          ORDER BY o.turn, o.created_at
          LIMIT $2",
    )
    .bind(grace_ms)
    .bind(limit.clamp(1, 10_000))
    .bind(per_project.clamp(1, 10_000))
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(OpenOperation {
                scope: ProjectScope {
                    project_id: row.try_get("project_id")?,
                    epoch: row.try_get("dataset_epoch")?,
                },
                id: row.try_get("id")?,
                payment_id: row.try_get("payment_id")?,
            })
        })
        .collect()
}

/// Records a failed check of an open operation: its next check waits `base` × 2^checks,
/// at most `max`. A resolved operation is left alone (it may have resolved meanwhile).
pub async fn postpone_check(
    pool: &PgPool,
    scope: ProjectScope,
    id: Uuid,
    base: std::time::Duration,
    max: std::time::Duration,
) -> Result<(), DbError> {
    let base_ms = i64::try_from(base.as_millis()).unwrap_or(i64::MAX);
    let max_ms = i64::try_from(max.as_millis()).unwrap_or(i64::MAX);
    sqlx::query(
        "UPDATE operations
            SET next_check_at = now() + make_interval(
                    secs => least($3::float8 * power(2, least(checks, 30)), $4::float8) / 1000.0),
                checks = checks + 1
          WHERE project_id = $1 AND id = $2 AND state IN ('pending', 'uncertain')",
    )
    .bind(scope.project_id)
    .bind(id)
    .bind(base_ms)
    .bind(max_ms)
    .execute(pool)
    .await?;
    Ok(())
}

/// How long the oldest open operation (of any project) has been open, if any is.
pub async fn oldest_open_age(pool: &PgPool) -> Result<Option<std::time::Duration>, DbError> {
    let ms: Option<i64> = sqlx::query_scalar(
        "SELECT (extract(epoch FROM now() - min(created_at)) * 1000)::bigint FROM operations
          WHERE state IN ('pending', 'uncertain')",
    )
    .fetch_one(pool)
    .await?;
    Ok(ms.map(|ms| std::time::Duration::from_millis(u64::try_from(ms).unwrap_or(0))))
}

/// Which of `ids` this project has no record of.
pub async fn unknown_operations(
    pool: &PgPool,
    project_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<Uuid>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT x FROM unnest($2::uuid[]) AS x
          WHERE NOT EXISTS (SELECT 1 FROM operations o WHERE o.project_id = $1 AND o.id = x)",
    )
    .bind(project_id)
    .bind(ids)
    .fetch_all(pool)
    .await?)
}

/// What the server recorded for an operation, for comparison with the network's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedOperation {
    /// Identity.
    pub id: Uuid,
    /// Kind code.
    pub kind: String,
    /// Amount.
    pub amount_minor: i64,
    /// State code.
    pub state: String,
    /// Decline code.
    pub decline_code: Option<String>,
}

/// The server's records of `ids` in `project_id` (any epoch).
pub async fn recorded_operations(
    pool: &PgPool,
    project_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<RecordedOperation>, DbError> {
    let rows = sqlx::query(
        "SELECT id, kind, amount_minor, state, decline_code FROM operations
          WHERE project_id = $1 AND id = ANY($2)",
    )
    .bind(project_id)
    .bind(ids)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(RecordedOperation {
                id: row.try_get("id")?,
                kind: row.try_get("kind")?,
                amount_minor: row.try_get("amount_minor")?,
                state: row.try_get("state")?,
                decline_code: row.try_get("decline_code")?,
            })
        })
        .collect()
}

/// A page of the ids of operations the network answered (succeeded or declined), in id
/// order after `after`.
pub async fn answered_operation_ids(
    pool: &PgPool,
    project_id: Uuid,
    after: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Uuid>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM operations
          WHERE project_id = $1 AND state IN ('succeeded', 'declined')
            AND ($2::uuid IS NULL OR id > $2)
          ORDER BY id LIMIT $3",
    )
    .bind(project_id)
    .bind(after)
    .bind(limit.clamp(1, 10_000))
    .fetch_all(pool)
    .await?)
}
