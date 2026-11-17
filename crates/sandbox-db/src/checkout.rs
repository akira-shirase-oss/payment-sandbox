//! Checkout sessions and their attempts (D-060, migration 0012).
//!
//! An attempt is committed under its session's row lock *before* any payment work, with
//! its authorize operation and payment ids derived from the session and the attempt
//! number, so the identity exists before the payer's click reaches the network. A new
//! attempt is admitted only while nothing of the session can still turn into money
//! ([`sandbox_domain::checkout::admits_new_attempt`]); the same client key always finds its
//! own attempt again.

use sandbox_domain::checkout::{self, CaptureMethod};
use sandbox_domain::identity::{self, IdempotencyKey};
use sandbox_domain::{Amount, Currency, OperationKind, OperationState};
use serde::Serialize;
use sqlx::{PgPool, Row, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DbError, ProjectScope};

macro_rules! session_columns {
    () => {
        "s.project_id, s.id, s.dataset_epoch, s.secret_hash, s.amount_minor, s.currency,
         s.description, s.capture_method, s.embed_origins, s.return_url, s.attempts,
         s.created_at, s.expires_at, s.expires_at <= now() AS expired, p.name AS merchant_name"
    };
}

macro_rules! attempt_columns {
    () => {
        "number, client_key, token, authorize_operation_id, payment_id, created_at"
    };
}

/// A checkout session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    /// Its project.
    pub project_id: Uuid,
    /// Id.
    pub id: Uuid,
    /// The dataset epoch it was created in (D-022).
    pub dataset_epoch: i32,
    /// SHA-256 of the client secret.
    #[serde(skip)]
    pub secret_hash: [u8; 32],
    /// Amount the merchant fixed.
    pub amount_minor: i64,
    /// Currency.
    pub currency: Currency,
    /// Description shown to the payer.
    pub description: Option<String>,
    /// When money is captured.
    pub capture_method: CaptureMethod,
    /// Exact origins that may frame the page.
    pub embed_origins: Vec<String>,
    /// Where the payer may go back to.
    pub return_url: Option<String>,
    /// Attempts started.
    pub attempts: i32,
    /// Created (database clock).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// No new attempt after this (database clock).
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// True when `expires_at` has passed, by the database clock at read time.
    pub expired: bool,
    /// The project's display name.
    pub merchant_name: String,
}

impl Session {
    /// The scope the session's payments are made in.
    pub const fn scope(&self) -> ProjectScope {
        ProjectScope {
            project_id: self.project_id,
            epoch: self.dataset_epoch,
        }
    }

    /// True when a new attempt is admitted by count and expiry.
    pub const fn can_start_new(&self) -> bool {
        !self.expired && self.attempts < checkout::MAX_ATTEMPTS
    }
}

/// A session to create.
#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    /// Id.
    pub id: Uuid,
    /// SHA-256 of the client secret.
    pub secret_hash: [u8; 32],
    /// Amount.
    pub amount: Amount,
    /// Currency.
    pub currency: Currency,
    /// Validated description.
    pub description: Option<&'a str>,
    /// Capture method.
    pub capture_method: CaptureMethod,
    /// Canonical embed origins.
    pub embed_origins: &'a [String],
    /// Validated return URL.
    pub return_url: Option<&'a str>,
    /// Lifetime in seconds (validated).
    pub lifetime_seconds: i64,
}

/// An attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attempt {
    /// 1-based number within its session.
    pub number: i32,
    /// The payer's `Idempotency-Key` for this attempt.
    #[serde(skip)]
    pub client_key: String,
    /// The scenario token chosen.
    pub token: String,
    /// The derived authorize operation.
    pub authorize_operation_id: Uuid,
    /// The derived payment.
    pub payment_id: Uuid,
    /// Created (database clock).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// What beginning an attempt found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Begun {
    /// A new attempt was committed; the caller drives it.
    Started(Attempt),
    /// The same client key and scenario were seen before: the caller drives that attempt
    /// again (a retry, never a second charge).
    Existing(Attempt),
}

/// Why an attempt was refused.
#[derive(Debug, thiserror::Error)]
pub enum AttemptError {
    /// The session does not exist.
    #[error("checkout session not found")]
    NotFound,
    /// The client key was used with another scenario.
    #[error("this Idempotency-Key was used with another scenario")]
    KeyConflict,
    /// The latest attempt may still turn into money (pending, uncertain or succeeded).
    #[error("the session's latest attempt is still open or succeeded")]
    Open,
    /// The network holds another request under an attempt's identity (a restore lost it):
    /// the session takes no new attempt (D-065).
    #[error("the session is closed: the network disagrees with its record of an attempt")]
    Closed,
    /// Every attempt was used.
    #[error("the session has used all of its attempts")]
    Exhausted,
    /// The session expired.
    #[error("the session has expired")]
    Expired,
    /// Storage.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for AttemptError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error.into())
    }
}

/// The derived authorize key, operation and payment of attempt `number`.
pub fn attempt_identity(session: &Session, number: i32) -> Result<(String, Uuid, Uuid), DbError> {
    let key = checkout::attempt_key(session.id, number);
    let parsed = IdempotencyKey::parse(&key)
        .map_err(|e| DbError::Constraint(format!("derived checkout key: {e}")))?;
    let operation = identity::operation_id(
        session.project_id,
        session.dataset_epoch,
        OperationKind::Authorize,
        &parsed,
    );
    Ok((key, operation, identity::payment_id(operation)))
}

/// Creates a session.
pub async fn create(
    pool: &PgPool,
    scope: ProjectScope,
    new: &NewSession<'_>,
) -> Result<Session, DbError> {
    sqlx::query(
        "INSERT INTO checkout_sessions (project_id, id, dataset_epoch, secret_hash, amount_minor,
                                        currency, description, capture_method, embed_origins,
                                        return_url, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                 now() + make_interval(secs => $11::float8))",
    )
    .bind(scope.project_id)
    .bind(new.id)
    .bind(scope.epoch)
    .bind(new.secret_hash.as_slice())
    .bind(new.amount.minor())
    .bind(new.currency.code())
    .bind(new.description)
    .bind(new.capture_method.code())
    .bind(new.embed_origins)
    .bind(new.return_url)
    .bind(i32::try_from(new.lifetime_seconds).unwrap_or(i32::MAX))
    .execute(pool)
    .await?;
    get(pool, scope, new.id).await?.ok_or(DbError::NotFound)
}

/// A session of `scope`'s project.
pub async fn get(pool: &PgPool, scope: ProjectScope, id: Uuid) -> Result<Option<Session>, DbError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        session_columns!(),
        " FROM checkout_sessions s JOIN projects p ON p.id = s.project_id
          WHERE s.project_id = $1 AND s.id = $2"
    ))
    .bind(scope.project_id)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(session_from).transpose()
}

/// A session by id alone: the payer's page knows nothing else, and its secret (checked by
/// the caller against [`Session::secret_hash`]) is what authorizes it.
pub async fn find(pool: &PgPool, id: Uuid) -> Result<Option<Session>, DbError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        session_columns!(),
        " FROM checkout_sessions s JOIN projects p ON p.id = s.project_id WHERE s.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(session_from).transpose()
}

/// Begins an attempt with the payer's `client_key` and scenario `token`, or finds the
/// attempt that key already began.
pub async fn begin_attempt(
    pool: &PgPool,
    session: &Session,
    client_key: &str,
    token: &str,
) -> Result<Begun, AttemptError> {
    let mut tx = pool.begin().await?;
    let locked = sqlx::query(
        "SELECT attempts, expires_at <= now() AS expired FROM checkout_sessions
          WHERE project_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(session.project_id)
    .bind(session.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AttemptError::NotFound)?;
    let attempts: i32 = locked.try_get("attempts")?;
    let expired: bool = locked.try_get("expired")?;
    // The same key finds its own attempt, whatever happened since (a retry is always
    // allowed; it re-drives that attempt and can never start a second one).
    let same = sqlx::query(concat!(
        "SELECT ",
        attempt_columns!(),
        " FROM checkout_attempts WHERE project_id = $1 AND session_id = $2 AND client_key = $3"
    ))
    .bind(session.project_id)
    .bind(session.id)
    .bind(client_key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = same {
        let attempt = attempt_from(&row)?;
        return if attempt.token == token {
            Ok(Begun::Existing(attempt))
        } else {
            Err(AttemptError::KeyConflict)
        };
    }
    let latest = sqlx::query(
        "SELECT o.state, o.decline_code FROM checkout_attempts a
           LEFT JOIN operations o ON o.project_id = a.project_id AND o.id = a.authorize_operation_id
          WHERE a.project_id = $1 AND a.session_id = $2
          ORDER BY a.number DESC LIMIT 1",
    )
    .bind(session.project_id)
    .bind(session.id)
    .fetch_optional(&mut *tx)
    .await?;
    let latest_state = match &latest {
        None => None,
        Some(row) => Some(
            row.try_get::<Option<String>, _>("state")?
                .map(|code| {
                    OperationState::from_code(&code)
                        .ok_or_else(|| DbError::Constraint(format!("state {code}")))
                })
                .transpose()?,
        ),
    };
    let conflicted = match &latest {
        Some(row) => {
            row.try_get::<Option<String>, _>("decline_code")?.as_deref()
                == Some(checkout::NETWORK_IDENTITY_CONFLICT)
        }
        None => false,
    };
    if conflicted {
        return Err(AttemptError::Closed);
    }
    if !checkout::admits_new_attempt(latest_state, conflicted) {
        return Err(AttemptError::Open);
    }
    if expired {
        return Err(AttemptError::Expired);
    }
    if attempts >= checkout::MAX_ATTEMPTS {
        return Err(AttemptError::Exhausted);
    }
    let number = attempts.saturating_add(1);
    let (_, operation, payment) = attempt_identity(session, number)?;
    let row = sqlx::query(concat!(
        "INSERT INTO checkout_attempts (project_id, session_id, number, client_key, token,
                                        authorize_operation_id, payment_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING ",
        attempt_columns!()
    ))
    .bind(session.project_id)
    .bind(session.id)
    .bind(number)
    .bind(client_key)
    .bind(token)
    .bind(operation)
    .bind(payment)
    .fetch_one(&mut *tx)
    .await?;
    let attempt = attempt_from(&row)?;
    sqlx::query("UPDATE checkout_sessions SET attempts = $3 WHERE project_id = $1 AND id = $2")
        .bind(session.project_id)
        .bind(session.id)
        .bind(number)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Begun::Started(attempt))
}

/// The session's attempts, oldest first (at most [`checkout::MAX_ATTEMPTS`]).
pub async fn attempts(pool: &PgPool, session: &Session) -> Result<Vec<Attempt>, DbError> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        attempt_columns!(),
        " FROM checkout_attempts WHERE project_id = $1 AND session_id = $2 ORDER BY number"
    ))
    .bind(session.project_id)
    .bind(session.id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(attempt_from).collect()
}

fn session_from(row: &PgRow) -> Result<Session, DbError> {
    let currency: String = row.try_get("currency")?;
    let method: String = row.try_get("capture_method")?;
    let hash: Vec<u8> = row.try_get("secret_hash")?;
    Ok(Session {
        project_id: row.try_get("project_id")?,
        id: row.try_get("id")?,
        dataset_epoch: row.try_get("dataset_epoch")?,
        secret_hash: hash
            .try_into()
            .map_err(|_| DbError::Constraint("secret_hash length".to_owned()))?,
        amount_minor: row.try_get("amount_minor")?,
        currency: Currency::from_code(&currency)
            .map_err(|_| DbError::Constraint(format!("currency {currency}")))?,
        description: row.try_get("description")?,
        capture_method: CaptureMethod::from_code(&method)
            .ok_or_else(|| DbError::Constraint(format!("capture_method {method}")))?,
        embed_origins: row.try_get("embed_origins")?,
        return_url: row.try_get("return_url")?,
        attempts: row.try_get("attempts")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        expired: row.try_get("expired")?,
        merchant_name: row.try_get("merchant_name")?,
    })
}

fn attempt_from(row: &PgRow) -> Result<Attempt, DbError> {
    Ok(Attempt {
        number: row.try_get("number")?,
        client_key: row.try_get("client_key")?,
        token: row.try_get("token")?,
        authorize_operation_id: row.try_get("authorize_operation_id")?,
        payment_id: row.try_get("payment_id")?,
        created_at: row.try_get("created_at")?,
    })
}
