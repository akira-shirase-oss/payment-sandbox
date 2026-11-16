//! Sandbox API keys and the audit trail.

use sandbox_domain::keys::{KeyHash, Scope};
use sqlx::{PgConnection, PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DbError, ProjectScope};

/// The identity a valid key resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyIdentity {
    /// The project and its current epoch.
    pub scope: ProjectScope,
    /// The key's id.
    pub key_id: Uuid,
    /// What it may do.
    pub scopes: Vec<Scope>,
}

/// A key as listed (never with its secret).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct KeyRecord {
    /// Id.
    pub id: Uuid,
    /// `sk_sim_` + 8 hex.
    pub prefix: String,
    /// Scope codes.
    pub scopes: Vec<String>,
    /// Label.
    pub label: String,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Revocation time, if revoked.
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// A new key to store.
#[derive(Debug, Clone)]
pub struct NewKey<'a> {
    /// Id.
    pub id: Uuid,
    /// SHA-256 of the secret.
    pub hash: KeyHash,
    /// Display prefix.
    pub prefix: &'a str,
    /// Scopes.
    pub scopes: &'a [Scope],
    /// Label.
    pub label: &'a str,
}

/// Inserts a key and its audit event in the caller's transaction.
pub async fn insert(
    conn: &mut PgConnection,
    project_id: Uuid,
    key: &NewKey<'_>,
    actor: &str,
) -> Result<KeyRecord, DbError> {
    let scopes: Vec<&str> = key.scopes.iter().map(|s| s.code()).collect();
    let row = sqlx::query(
        "INSERT INTO api_keys (project_id, id, key_hash, key_prefix, scopes, label)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id, key_prefix, scopes, label, created_at, revoked_at",
    )
    .bind(project_id)
    .bind(key.id)
    .bind(key.hash.0.as_slice())
    .bind(key.prefix)
    .bind(&scopes)
    .bind(key.label)
    .fetch_one(&mut *conn)
    .await?;
    audit(conn, project_id, actor, "key.created", Some(key.id)).await?;
    record(&row)
}

/// Resolves a presented key's hash to its identity; `None` for unknown or revoked keys.
pub async fn resolve(pool: &PgPool, hash: &KeyHash) -> Result<Option<KeyIdentity>, DbError> {
    let row = sqlx::query(
        "SELECT k.project_id, k.id, k.scopes, p.dataset_epoch
           FROM api_keys k JOIN projects p ON p.id = k.project_id
          WHERE k.key_hash = $1 AND k.revoked_at IS NULL",
    )
    .bind(hash.0.as_slice())
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let codes: Vec<String> = row.try_get("scopes")?;
    Ok(Some(KeyIdentity {
        scope: ProjectScope {
            project_id: row.try_get("project_id")?,
            epoch: row.try_get("dataset_epoch")?,
        },
        key_id: row.try_get("id")?,
        scopes: codes.iter().filter_map(|c| Scope::from_code(c)).collect(),
    }))
}

/// Every key of the project, newest first.
pub async fn list(pool: &PgPool, project_id: Uuid) -> Result<Vec<KeyRecord>, DbError> {
    let rows = sqlx::query(
        "SELECT id, key_prefix, scopes, label, created_at, revoked_at
           FROM api_keys WHERE project_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1000",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(record).collect()
}

/// Why a revocation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// Revoked now (or already revoked: revocation is idempotent).
    Revoked,
    /// No such key in this project.
    NotFound,
    /// It is the project's last active admin key; revoking it would lock the project out.
    LastAdmin,
}

/// Revokes a key of `project_id`, refusing to remove the last active admin key.
pub async fn revoke(
    pool: &PgPool,
    project_id: Uuid,
    key_id: Uuid,
    actor: &str,
) -> Result<RevokeOutcome, DbError> {
    let mut tx = pool.begin().await?;
    // Lock every active admin key of the project, so two concurrent revocations cannot
    // each see "another admin remains" and both succeed.
    let admins: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM api_keys
          WHERE project_id = $1 AND revoked_at IS NULL AND 'admin' = ANY(scopes)
          ORDER BY id FOR UPDATE",
    )
    .bind(project_id)
    .fetch_all(&mut *tx)
    .await?;
    let target =
        sqlx::query("SELECT revoked_at FROM api_keys WHERE project_id = $1 AND id = $2 FOR UPDATE")
            .bind(project_id)
            .bind(key_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(target) = target else {
        return Ok(RevokeOutcome::NotFound);
    };
    let revoked_at: Option<OffsetDateTime> = target.try_get("revoked_at")?;
    if revoked_at.is_some() {
        return Ok(RevokeOutcome::Revoked);
    }
    if admins.contains(&key_id) && admins.len() == 1 {
        return Ok(RevokeOutcome::LastAdmin);
    }
    sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE project_id = $1 AND id = $2")
        .bind(project_id)
        .bind(key_id)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, project_id, actor, "key.revoked", Some(key_id)).await?;
    tx.commit().await?;
    Ok(RevokeOutcome::Revoked)
}

/// Appends an audit event.
pub async fn audit(
    conn: &mut PgConnection,
    project_id: Uuid,
    actor: &str,
    action: &str,
    subject: Option<Uuid>,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO audit_events (project_id, id, actor, action, subject_id) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(project_id)
    .bind(Uuid::now_v7())
    .bind(actor)
    .bind(action)
    .bind(subject)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn record(row: &sqlx::postgres::PgRow) -> Result<KeyRecord, DbError> {
    Ok(KeyRecord {
        id: row.try_get("id")?,
        prefix: row.try_get("key_prefix")?,
        scopes: row.try_get("scopes")?,
        label: row.try_get("label")?,
        created_at: row.try_get("created_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}
