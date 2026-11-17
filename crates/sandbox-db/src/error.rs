//! One error type for storage, classified so the API can answer honestly: an unavailable
//! database is `503`, a journal refusal is a defect, a unique violation is a conflict.

/// Storage failures.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The journal's own rules refused a write (SQLSTATE PSJ01–PSJ03). Always a defect in
    /// the caller: the domain never builds such an entry.
    #[error("the journal refused the write ({0})")]
    JournalRefused(&'static str),
    /// A row the caller needs does not exist (in this project).
    #[error("not found")]
    NotFound,
    /// A cursor taken before the database was restored (D-050): its sequence numbers and
    /// snapshot belong to another history.
    #[error("the cursor is from before a restore of the database")]
    StaleCursor,
    /// A unique or check constraint refused the write.
    #[error("constraint refused the write: {0}")]
    Constraint(String),
    /// The database could not be reached in time (pool timeout, connection refused, I/O).
    #[error("database unavailable")]
    Unavailable(#[source] sqlx::Error),
    /// A statement or lock exceeded its deadline.
    #[error("database deadline exceeded")]
    Timeout(#[source] sqlx::Error),
    /// A serialization failure or deadlock; the caller may retry the whole transaction.
    #[error("transaction conflict; retry")]
    Retryable(#[source] sqlx::Error),
    /// Anything else.
    #[error("database error")]
    Other(#[source] sqlx::Error),
}

impl DbError {
    /// True for failures a retry of the same transaction may clear: an unavailable pool or
    /// connection, a statement or lock deadline, a serialization failure or deadlock.
    pub const fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Unavailable(_) | Self::Timeout(_) | Self::Retryable(_)
        )
    }

    /// The PostgreSQL SQLSTATE of the underlying error, if any.
    pub fn sqlstate(&self) -> Option<String> {
        match self {
            Self::Unavailable(e) | Self::Timeout(e) | Self::Retryable(e) | Self::Other(e) => e
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .map(std::borrow::Cow::into_owned),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for DbError {
    fn from(error: sqlx::Error) -> Self {
        match &error {
            sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::Io(_)
            | sqlx::Error::Tls(_) => Self::Unavailable(error),
            sqlx::Error::RowNotFound => Self::NotFound,
            sqlx::Error::Database(db) => {
                let code = db
                    .code()
                    .map(std::borrow::Cow::into_owned)
                    .unwrap_or_default();
                match code.as_str() {
                    "PSJ01" => Self::JournalRefused("append-only"),
                    "PSJ02" => Self::JournalRefused("unbalanced or foreign lines"),
                    "PSJ03" => Self::JournalRefused("entry without lines"),
                    "PSO01" => Self::JournalRefused("operation already resolved"),
                    // unique, foreign key, check, not null
                    "23505" | "23503" | "23514" | "23502" => {
                        let constraint = db.constraint().unwrap_or("unnamed").to_owned();
                        Self::Constraint(constraint)
                    }
                    // query_canceled (statement_timeout), lock_not_available (lock_timeout)
                    "57014" | "55P03" => Self::Timeout(error),
                    // serialization_failure, deadlock_detected
                    "40001" | "40P01" => Self::Retryable(error),
                    // admin_shutdown, crash_shutdown, cannot_connect_now
                    "57P01" | "57P02" | "57P03" => Self::Unavailable(error),
                    _ => Self::Other(error),
                }
            }
            _ => Self::Other(error),
        }
    }
}
