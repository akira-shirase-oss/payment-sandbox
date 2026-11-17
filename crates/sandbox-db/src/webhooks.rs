//! Webhook endpoints, the event outbox and leased deliveries (D-006, D-032).
//!
//! Times that decide leases and retries are passed in by the caller (`now`) rather than
//! read from the database clock, so a test can jump the deliverer's clock (drill (b), the
//! wall-clock half) and prove a jump causes at most a duplicate delivery: every claim bumps
//! `lease_token`, and only the holder of the current token can record a result.

use serde::Serialize;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DbError, ProjectScope};

/// An endpoint as listed (never with its secret).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Endpoint {
    /// Id.
    pub id: Uuid,
    /// Target URL (loopback only, D-023).
    pub url: String,
    /// True while a rotated-out secret still signs.
    pub rotating: bool,
    /// Created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Disabled, if it is.
    #[serde(with = "time::serde::rfc3339::option")]
    pub disabled_at: Option<OffsetDateTime>,
    /// Failed attempts since the last success (D-049).
    pub consecutive_failures: i32,
    /// While set and in the future, nothing is sent to this endpoint (its circuit is open).
    #[serde(with = "time::serde::rfc3339::option")]
    pub circuit_open_until: Option<OffsetDateTime>,
}

/// An encrypted secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// Ciphertext with tag.
    pub ciphertext: Vec<u8>,
    /// 12-byte nonce.
    pub nonce: Vec<u8>,
}

/// Creates an endpoint.
pub async fn create_endpoint(
    pool: &PgPool,
    scope: ProjectScope,
    id: Uuid,
    url: &str,
    secret: &Sealed,
) -> Result<Endpoint, DbError> {
    let row = sqlx::query(
        "INSERT INTO webhook_endpoints (project_id, id, url, secret_ciphertext, secret_nonce)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, url, previous_ciphertext IS NOT NULL AS rotating, created_at, disabled_at,
                   consecutive_failures, circuit_open_until",
    )
    .bind(scope.project_id)
    .bind(id)
    .bind(url)
    .bind(&secret.ciphertext)
    .bind(&secret.nonce)
    .fetch_one(pool)
    .await?;
    endpoint_from(&row)
}

/// The project's endpoints.
pub async fn list_endpoints(pool: &PgPool, scope: ProjectScope) -> Result<Vec<Endpoint>, DbError> {
    let rows = sqlx::query(
        "SELECT id, url, previous_ciphertext IS NOT NULL AND previous_expires_at > now() AS rotating,
                created_at, disabled_at, consecutive_failures, circuit_open_until
           FROM webhook_endpoints WHERE project_id = $1 ORDER BY created_at LIMIT 100",
    )
    .bind(scope.project_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(endpoint_from).collect()
}

/// Replaces the secret; the old one keeps signing until `previous_expires_at`.
pub async fn rotate(
    pool: &PgPool,
    scope: ProjectScope,
    id: Uuid,
    next: &Sealed,
    previous_valid_for: std::time::Duration,
) -> Result<bool, DbError> {
    let ms = i64::try_from(previous_valid_for.as_millis()).unwrap_or(i64::MAX);
    let updated = sqlx::query(
        "UPDATE webhook_endpoints
            SET previous_ciphertext = secret_ciphertext, previous_nonce = secret_nonce,
                previous_expires_at = now() + make_interval(secs => $5::float8 / 1000.0),
                secret_ciphertext = $3, secret_nonce = $4
          WHERE project_id = $1 AND id = $2 AND disabled_at IS NULL",
    )
    .bind(scope.project_id)
    .bind(id)
    .bind(&next.ciphertext)
    .bind(&next.nonce)
    .bind(ms)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(updated == 1)
}

/// Disables an endpoint: no new deliveries are created for it.
pub async fn disable(pool: &PgPool, scope: ProjectScope, id: Uuid) -> Result<bool, DbError> {
    let updated = sqlx::query(
        "UPDATE webhook_endpoints SET disabled_at = coalesce(disabled_at, now())
          WHERE project_id = $1 AND id = $2",
    )
    .bind(scope.project_id)
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(updated == 1)
}

/// A claimed delivery, with what is needed to send it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    /// Project.
    pub project_id: Uuid,
    /// Event.
    pub event_id: Uuid,
    /// Endpoint.
    pub endpoint_id: Uuid,
    /// The fencing token of this claim.
    pub lease_token: i64,
    /// Attempts so far.
    pub attempts: i32,
    /// Target URL.
    pub url: String,
    /// Current secret.
    pub secret: Sealed,
    /// Rotated-out secret still signing, if any.
    pub previous: Option<Sealed>,
    /// Event fields.
    pub kind: String,
    /// Payment.
    pub payment_id: Uuid,
    /// Operation.
    pub operation_id: Uuid,
    /// Per-payment sequence.
    pub sequence: i64,
    /// Event time, Unix seconds.
    pub created_at: i64,
}

/// Attempts already in flight to one endpoint, in this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    /// The endpoint's project.
    pub project_id: Uuid,
    /// The endpoint.
    pub endpoint_id: Uuid,
    /// Attempts in flight.
    pub in_flight: i32,
}

/// The circuit breaker (D-049): `threshold` failures in a row open an endpoint's circuit for
/// `cooldown`, doubling with each opening up to `max_cooldown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Breaker {
    /// Consecutive failures that open the circuit.
    pub threshold: i32,
    /// First cooldown.
    pub cooldown: std::time::Duration,
    /// Longest cooldown.
    pub max_cooldown: std::time::Duration,
}

/// Claims up to `limit` due deliveries under a lease ending `lease` after `now`, **taking
/// the endpoints in turn** (review finding 12): every endpoint's most overdue delivery,
/// then every endpoint's second, and so on, never more than `per_endpoint` in flight to
/// one endpoint counting those in `busy`. Endpoints whose circuit is open are skipped; one
/// whose circuit is half-open (at or past `breaker.threshold` failures, cooldown over) gets
/// one attempt at a time. Disabled endpoints get none (review 18).
pub async fn claim(
    pool: &PgPool,
    now: OffsetDateTime,
    lease: std::time::Duration,
    limit: i64,
    per_endpoint: i64,
    breaker: Breaker,
    busy: &[Busy],
) -> Result<Vec<Claimed>, DbError> {
    let lease_ms = i64::try_from(lease.as_millis()).unwrap_or(i64::MAX);
    let busy_projects: Vec<Uuid> = busy.iter().map(|b| b.project_id).collect();
    let busy_endpoints: Vec<Uuid> = busy.iter().map(|b| b.endpoint_id).collect();
    let busy_counts: Vec<i32> = busy.iter().map(|b| b.in_flight).collect();
    let rows = sqlx::query(
        "WITH busy AS (
             SELECT * FROM unnest($5::uuid[], $6::uuid[], $7::int[]) AS b(project_id, endpoint_id, in_flight)
         ), room AS (
             SELECT w.project_id, w.id AS endpoint_id,
                    (CASE WHEN w.consecutive_failures >= $8 THEN 1 ELSE $4 END)
                        - coalesce(b.in_flight, 0) AS room
               FROM webhook_endpoints w
               LEFT JOIN busy b ON b.project_id = w.project_id AND b.endpoint_id = w.id
              WHERE w.disabled_at IS NULL
                AND (w.circuit_open_until IS NULL OR w.circuit_open_until <= $1)
         ), due AS (
             SELECT c.project_id, c.event_id, c.endpoint_id
               FROM room r
              CROSS JOIN LATERAL (
                  SELECT l.project_id, l.event_id, l.endpoint_id, l.next_attempt_at,
                         row_number() OVER (ORDER BY l.next_attempt_at, l.event_id) AS turn
                    FROM (SELECT d.project_id, d.event_id, d.endpoint_id, d.next_attempt_at
                            FROM deliveries d
                           WHERE d.project_id = r.project_id AND d.endpoint_id = r.endpoint_id
                             AND d.state = 'pending' AND d.next_attempt_at <= $1
                             AND (d.lease_until IS NULL OR d.lease_until <= $1)
                           ORDER BY d.next_attempt_at, d.event_id
                           LIMIT $4
                           FOR UPDATE OF d SKIP LOCKED) l
              ) c
              WHERE r.room > 0 AND c.turn <= r.room
              ORDER BY c.turn, c.next_attempt_at
              LIMIT $3
         )
         UPDATE deliveries d
            SET lease_until = $1 + make_interval(secs => $2::float8 / 1000.0),
                lease_token = d.lease_token + 1
           FROM due, events e, webhook_endpoints w
          WHERE d.project_id = due.project_id AND d.event_id = due.event_id AND d.endpoint_id = due.endpoint_id
            AND e.project_id = d.project_id AND e.id = d.event_id
            AND w.project_id = d.project_id AND w.id = d.endpoint_id
         RETURNING d.project_id, d.event_id, d.endpoint_id, d.lease_token, d.attempts,
                   w.url, w.secret_ciphertext, w.secret_nonce,
                   CASE WHEN w.previous_expires_at > $1 THEN w.previous_ciphertext END AS previous_ciphertext,
                   CASE WHEN w.previous_expires_at > $1 THEN w.previous_nonce END AS previous_nonce,
                   e.kind, e.payment_id, e.operation_id, e.sequence,
                   extract(epoch FROM e.created_at)::bigint AS created_at",
    )
    .bind(now)
    .bind(lease_ms)
    .bind(limit.clamp(1, 1_000))
    .bind(per_endpoint.clamp(1, 1_000))
    .bind(&busy_projects)
    .bind(&busy_endpoints)
    .bind(&busy_counts)
    .bind(breaker.threshold)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let previous_ciphertext: Option<Vec<u8>> = row.try_get("previous_ciphertext")?;
            let previous_nonce: Option<Vec<u8>> = row.try_get("previous_nonce")?;
            Ok(Claimed {
                project_id: row.try_get("project_id")?,
                event_id: row.try_get("event_id")?,
                endpoint_id: row.try_get("endpoint_id")?,
                lease_token: row.try_get("lease_token")?,
                attempts: row.try_get("attempts")?,
                url: row.try_get("url")?,
                secret: Sealed {
                    ciphertext: row.try_get("secret_ciphertext")?,
                    nonce: row.try_get("secret_nonce")?,
                },
                previous: previous_ciphertext
                    .zip(previous_nonce)
                    .map(|(ciphertext, nonce)| Sealed { ciphertext, nonce }),
                kind: row.try_get("kind")?,
                payment_id: row.try_get("payment_id")?,
                operation_id: row.try_get("operation_id")?,
                sequence: row.try_get("sequence")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// 2xx.
    Delivered {
        /// Receiver status.
        status: u16,
    },
    /// Failed; try again at `next_at`.
    Retry {
        /// When.
        next_at: OffsetDateTime,
        /// Receiver status, if any.
        status: Option<u16>,
        /// Short reason (no payloads, no secrets).
        error: String,
    },
    /// Failed for the last time.
    Dead {
        /// Receiver status, if any.
        status: Option<u16>,
        /// Short reason.
        error: String,
    },
}

/// Records an attempt, only if `lease_token` is still the current one, and in the same
/// statement the endpoint's breaker: a success closes its circuit; a failure counts, and the
/// failure that reaches `breaker.threshold` opens the circuit (unless it is already open)
/// for the next cooldown. Returns false for a stale worker, whose result is discarded and
/// counts for nothing.
pub async fn complete(
    pool: &PgPool,
    claimed: &Claimed,
    attempt: &Attempt,
    now: OffsetDateTime,
    breaker: Breaker,
) -> Result<bool, DbError> {
    let (state, next_at, status, error) = match attempt {
        Attempt::Delivered { status } => ("delivered", now, Some(i32::from(*status)), None),
        Attempt::Retry {
            next_at,
            status,
            error,
        } => (
            "pending",
            *next_at,
            status.map(i32::from),
            Some(error.as_str()),
        ),
        Attempt::Dead { status, error } => {
            ("dead", now, status.map(i32::from), Some(error.as_str()))
        }
    };
    let cooldown_ms = i64::try_from(breaker.cooldown.as_millis()).unwrap_or(i64::MAX);
    let max_ms = i64::try_from(breaker.max_cooldown.as_millis()).unwrap_or(i64::MAX);
    let recorded = sqlx::query(
        "WITH done AS (
             UPDATE deliveries
                SET state = $5, attempts = attempts + 1, next_attempt_at = $6, lease_until = NULL,
                    last_status = $7, last_error = left($8, 300),
                    delivered_at = CASE WHEN $5 = 'delivered' THEN $9 ELSE delivered_at END
              WHERE project_id = $1 AND event_id = $2 AND endpoint_id = $3 AND lease_token = $4
                AND state = 'pending'
             RETURNING project_id, endpoint_id
         )
         -- Computed from the endpoint row itself, which a concurrent completion's update
         -- makes PostgreSQL re-read: two failures crossing the threshold together open the
         -- circuit once.
         UPDATE webhook_endpoints w
            SET consecutive_failures = CASE WHEN $5 = 'delivered' THEN 0
                                            ELSE w.consecutive_failures + 1 END,
                circuit_opens = CASE
                    WHEN $5 = 'delivered' THEN 0
                    WHEN w.consecutive_failures + 1 >= $10
                         AND (w.circuit_open_until IS NULL OR w.circuit_open_until <= $9)
                    THEN w.circuit_opens + 1
                    ELSE w.circuit_opens END,
                circuit_open_until = CASE
                    WHEN $5 = 'delivered' THEN NULL
                    WHEN w.consecutive_failures + 1 >= $10
                         AND (w.circuit_open_until IS NULL OR w.circuit_open_until <= $9)
                    THEN $9 + make_interval(secs => least(
                        $11::float8 * power(2, least(w.circuit_opens, 20)), $12::float8) / 1000.0)
                    ELSE w.circuit_open_until END
           FROM done
          WHERE w.project_id = done.project_id AND w.id = done.endpoint_id
         RETURNING w.id",
    )
    .bind(claimed.project_id)
    .bind(claimed.event_id)
    .bind(claimed.endpoint_id)
    .bind(claimed.lease_token)
    .bind(state)
    .bind(next_at)
    .bind(status)
    .bind(error)
    .bind(now)
    .bind(breaker.threshold)
    .bind(cooldown_ms)
    .bind(max_ms)
    .fetch_optional(pool)
    .await?;
    Ok(recorded.is_some())
}

/// An event with its delivery states, for inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventRecord {
    /// Event id.
    pub id: Uuid,
    /// Kind.
    pub kind: String,
    /// Payment.
    pub payment_id: Uuid,
    /// Operation.
    pub operation_id: Uuid,
    /// Sequence.
    pub sequence: i64,
    /// Unix seconds.
    pub created_at: i64,
    /// `(endpoint, state, attempts)` per delivery.
    pub deliveries: Vec<(Uuid, String, i32)>,
}

/// The project's most recent events (optionally of one payment), newest first.
pub async fn events(
    pool: &PgPool,
    scope: ProjectScope,
    payment_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<EventRecord>, DbError> {
    let rows = sqlx::query(
        "SELECT e.id, e.kind, e.payment_id, e.operation_id, e.sequence,
                extract(epoch FROM e.created_at)::bigint AS created_at,
                coalesce(array_agg(d.endpoint_id) FILTER (WHERE d.endpoint_id IS NOT NULL), '{}') AS endpoints,
                coalesce(array_agg(d.state) FILTER (WHERE d.endpoint_id IS NOT NULL), '{}') AS states,
                coalesce(array_agg(d.attempts) FILTER (WHERE d.endpoint_id IS NOT NULL), '{}') AS attempts
           FROM events e
           LEFT JOIN deliveries d ON d.project_id = e.project_id AND d.event_id = e.id
          WHERE e.project_id = $1 AND e.dataset_epoch = $2 AND ($3::uuid IS NULL OR e.payment_id = $3)
          GROUP BY e.project_id, e.id
          ORDER BY e.created_at DESC, e.sequence DESC
          LIMIT $4",
    )
    .bind(scope.project_id)
    .bind(scope.epoch)
    .bind(payment_id)
    .bind(limit.clamp(1, 1_000))
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let endpoints: Vec<Uuid> = row.try_get("endpoints")?;
            let states: Vec<String> = row.try_get("states")?;
            let attempts: Vec<i32> = row.try_get("attempts")?;
            Ok(EventRecord {
                id: row.try_get("id")?,
                kind: row.try_get("kind")?,
                payment_id: row.try_get("payment_id")?,
                operation_id: row.try_get("operation_id")?,
                sequence: row.try_get("sequence")?,
                created_at: row.try_get("created_at")?,
                deliveries: endpoints
                    .into_iter()
                    .zip(states)
                    .zip(attempts)
                    .map(|((e, s), a)| (e, s, a))
                    .collect(),
            })
        })
        .collect()
}

fn endpoint_from(row: &sqlx::postgres::PgRow) -> Result<Endpoint, DbError> {
    Ok(Endpoint {
        id: row.try_get("id")?,
        url: row.try_get("url")?,
        rotating: row.try_get::<Option<bool>, _>("rotating")?.unwrap_or(false),
        created_at: row.try_get("created_at")?,
        disabled_at: row.try_get("disabled_at")?,
        consecutive_failures: row.try_get("consecutive_failures")?,
        circuit_open_until: row.try_get("circuit_open_until")?,
    })
}
