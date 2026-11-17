//! Projects and their chart of accounts.

use sandbox_domain::Currency;
use sandbox_domain::account::CHART;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DbError, ProjectScope};

/// A project row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Id.
    pub id: Uuid,
    /// URL-safe unique name.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Current dataset epoch.
    pub dataset_epoch: i32,
    /// Creation time (database clock).
    pub created_at: OffsetDateTime,
}

impl Project {
    /// The scope for this project's current epoch.
    pub const fn scope(&self) -> ProjectScope {
        ProjectScope {
            project_id: self.id,
            epoch: self.dataset_epoch,
        }
    }
}

/// Creates a project and its full chart of accounts (every kind in every currency) in one
/// transaction.
pub async fn create(pool: &PgPool, id: Uuid, slug: &str, name: &str) -> Result<Project, DbError> {
    let mut tx = pool.begin().await?;
    let project = create_in(&mut tx, id, slug, name).await?;
    tx.commit().await?;
    Ok(project)
}

/// [`create`] inside the caller's transaction (with the project's first key, for example).
pub async fn create_in(
    tx: &mut sqlx::PgConnection,
    id: Uuid,
    slug: &str,
    name: &str,
) -> Result<Project, DbError> {
    let row = sqlx::query(
        "INSERT INTO projects (id, slug, name) VALUES ($1, $2, $3)
         RETURNING id, slug, name, dataset_epoch, created_at",
    )
    .bind(id)
    .bind(slug)
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    crate::keys::audit(tx, id, "bootstrap", "project.created", Some(id)).await?;
    let (currencies, kinds): (Vec<&str>, Vec<&str>) = Currency::ALL
        .iter()
        .flat_map(|c| CHART.iter().map(move |k| (c.code(), k.code())))
        .unzip();
    sqlx::query(
        "INSERT INTO accounts (project_id, currency, kind)
         SELECT $1, c, k FROM unnest($2::text[], $3::text[]) AS t(c, k)",
    )
    .bind(id)
    .bind(&currencies)
    .bind(&kinds)
    .execute(&mut *tx)
    .await?;
    project_from_row(&row)
}

/// Loads a project by id.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Project, DbError> {
    let row =
        sqlx::query("SELECT id, slug, name, dataset_epoch, created_at FROM projects WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or(DbError::NotFound)?;
    project_from_row(&row)
}

fn project_from_row(row: &sqlx::postgres::PgRow) -> Result<Project, DbError> {
    Ok(Project {
        id: row.try_get("id")?,
        slug: row.try_get("slug")?,
        name: row.try_get("name")?,
        dataset_epoch: row.try_get("dataset_epoch")?,
        created_at: row.try_get("created_at")?,
    })
}

/// What a dataset reset did (D-022, D-066).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetOutcome {
    /// The project moved from epoch `from` to `to = from + 1`; `archived_payments` payments
    /// stay in epoch `from`, with every journal entry, event and operation of it.
    Reset {
        /// The epoch that was current.
        from: i32,
        /// The new current epoch.
        to: i32,
        /// Payments of the archived epoch.
        archived_payments: i64,
    },
    /// The typed confirmation is not the project's slug; nothing changed.
    ConfirmationMismatch,
    /// The caller named an epoch that is no longer current (another reset, or this one
    /// already applied); nothing changed.
    EpochChanged {
        /// The epoch now current.
        current: i32,
    },
}

/// Opens a new dataset epoch for `project_id` if `confirm` is its slug and `expected` its
/// current epoch, writing a `dataset.reset` audit event in the same transaction. Deletes
/// and changes nothing of the earlier epoch; the database refuses any other change to a
/// project (migration 0013).
pub async fn reset(
    pool: &PgPool,
    project_id: Uuid,
    expected: i32,
    confirm: &str,
    actor: &str,
    key_id: Uuid,
) -> Result<ResetOutcome, DbError> {
    let mut tx = pool.begin().await?;
    // The row lock serializes resets of one project; reads (key lookups) are not blocked.
    let row = sqlx::query("SELECT slug, dataset_epoch FROM projects WHERE id = $1 FOR UPDATE")
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(DbError::NotFound)?;
    let slug: String = row.try_get("slug")?;
    let current: i32 = row.try_get("dataset_epoch")?;
    if slug != confirm {
        return Ok(ResetOutcome::ConfirmationMismatch);
    }
    if current != expected {
        return Ok(ResetOutcome::EpochChanged { current });
    }
    let to: i32 = sqlx::query_scalar(
        "UPDATE projects SET dataset_epoch = dataset_epoch + 1 WHERE id = $1 RETURNING dataset_epoch",
    )
    .bind(project_id)
    .fetch_one(&mut *tx)
    .await?;
    crate::keys::audit(&mut tx, project_id, actor, "dataset.reset", Some(key_id)).await?;
    tx.commit().await?;
    // Counted after the commit, on the archived epoch (which no longer changes but for
    // operations still in flight), so the reset holds its lock only as long as it must.
    let archived_payments: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM payments WHERE project_id = $1 AND dataset_epoch = $2",
    )
    .bind(project_id)
    .bind(current)
    .fetch_one(pool)
    .await?;
    Ok(ResetOutcome::Reset {
        from: current,
        to,
        archived_payments,
    })
}
