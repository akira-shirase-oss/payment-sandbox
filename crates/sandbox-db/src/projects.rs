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
    let row = sqlx::query(
        "INSERT INTO projects (id, slug, name) VALUES ($1, $2, $3)
         RETURNING id, slug, name, dataset_epoch, created_at",
    )
    .bind(id)
    .bind(slug)
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
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
    tx.commit().await?;
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
