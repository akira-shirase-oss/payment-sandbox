//! Database roles (review finding 1; D-038). The server never connects as a superuser or
//! as the owner of the tables: either could switch off the journal's triggers
//! (`session_replication_role`, `DISABLE TRIGGER`) or truncate it.
//!
//! - `ps_owner` owns the database and every object in it; only migrations use it.
//! - `ps_runtime` is a group role with exactly the grants the server needs
//!   (`migrations/0005_runtime_role.sql`): no `TRUNCATE`, no DDL, no `TEMP`.
//! - `ps_app` is the server's login, a member of `ps_runtime`, `NOSUPERUSER NOBYPASSRLS`.
//!
//! The passwords below are public development values for the local and CI stacks (D-023).

use sqlx::{AssertSqlSafe, Executor, PgConnection, PgPool};

use crate::DbError;

/// Owner of every object; migrations only.
pub const OWNER: &str = "ps_owner";
/// Owner's development password.
pub const OWNER_PASSWORD: &str = "ps-owner-dev";
/// Group role holding the server's grants.
pub const RUNTIME: &str = "ps_runtime";
/// The server's login.
pub const APP: &str = "ps_app";
/// The server's development password.
pub const APP_PASSWORD: &str = "ps-app-dev";

/// Creates (or updates) the three roles. Needs a role with `CREATEROLE` (the stack's
/// superuser). Serialized by a transaction-scoped advisory lock (held until commit), so
/// parallel tests may call it.
pub async fn bootstrap(admin: &mut PgConnection) -> Result<(), DbError> {
    admin
        .execute(AssertSqlSafe(format!(
            "BEGIN;
             SELECT pg_advisory_xact_lock(4242001);
             DO $$ BEGIN
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{OWNER}') THEN
                 CREATE ROLE {OWNER} LOGIN NOSUPERUSER NOCREATEROLE NOBYPASSRLS;
               END IF;
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{RUNTIME}') THEN
                 CREATE ROLE {RUNTIME} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;
               END IF;
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{APP}') THEN
                 CREATE ROLE {APP} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS IN ROLE {RUNTIME};
               END IF;
             END $$;
             ALTER ROLE {OWNER} PASSWORD '{OWNER_PASSWORD}';
             ALTER ROLE {APP} PASSWORD '{APP_PASSWORD}';
             COMMIT;"
        )))
        .await?;
    Ok(())
}

/// Creates `database` owned by `ps_owner`, closed to everyone but the owner and
/// `ps_runtime` (which may connect but not create temporary tables).
pub async fn create_database(admin: &mut PgConnection, database: &str) -> Result<(), DbError> {
    if !database
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(DbError::Constraint(
            "database names are [a-z0-9_]".to_owned(),
        ));
    }
    admin
        .execute(AssertSqlSafe(format!(
            "CREATE DATABASE {database} OWNER {OWNER}"
        )))
        .await?;
    admin
        .execute(AssertSqlSafe(format!(
            "REVOKE ALL ON DATABASE {database} FROM PUBLIC;
             GRANT CONNECT ON DATABASE {database} TO {RUNTIME};"
        )))
        .await?;
    Ok(())
}

/// Why the server refuses to run with this connection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleProblem {
    /// A superuser can switch the journal's triggers off.
    #[error("refusing to run as a superuser ({0}): connect as the runtime role ps_app")]
    Superuser(String),
    /// A role that bypasses row-level security.
    #[error("refusing to run as a role with BYPASSRLS ({0})")]
    BypassRls(String),
    /// The owner can disable triggers and truncate.
    #[error(
        "refusing to run as the owner of the journal ({0}): connect as the runtime role ps_app"
    )]
    Owner(String),
    /// TEMP lets a role shadow tables in the triggers' queries.
    #[error("refusing to run as a role that may create temporary tables ({0})")]
    Temporary(String),
    /// The journal's protections are not as the migrations left them (review N5, D-058).
    #[error("refusing to run: the database's protections were weakened: {0}")]
    Unprotected(String),
}

/// The protection manifest (D-058): one row per problem, none when protected.
pub const PROTECTIONS: &str = include_str!("../sql/protections.sql");

/// What [`PROTECTIONS`] finds wrong with the database behind `pool` (empty when protected).
pub async fn protection_problems(pool: &PgPool) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(PROTECTIONS))
        .fetch_all(pool)
        .await?)
}

/// Checks the pool's role, then the journal's protections (D-058), before the server
/// serves anything.
pub async fn check_runtime(pool: &PgPool) -> Result<(), RoleCheckError> {
    let (name, superuser, bypass, owner, temp): (String, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT current_user::text, r.rolsuper, r.rolbypassrls,
                coalesce((SELECT pg_has_role(current_user, c.relowner, 'USAGE')
                            FROM pg_class c WHERE c.oid = 'journal_lines'::regclass), false),
                has_database_privilege(current_user, current_database(), 'TEMP')
           FROM pg_roles r WHERE r.rolname = current_user",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| RoleCheckError::Db(e.into()))?;
    let problem = if superuser {
        Some(RoleProblem::Superuser(name))
    } else if bypass {
        Some(RoleProblem::BypassRls(name))
    } else if owner {
        Some(RoleProblem::Owner(name))
    } else if temp {
        Some(RoleProblem::Temporary(name))
    } else {
        None
    };
    if let Some(problem) = problem {
        return Err(RoleCheckError::Refused(problem));
    }
    let problems = protection_problems(pool)
        .await
        .map_err(RoleCheckError::Db)?;
    if problems.is_empty() {
        Ok(())
    } else {
        Err(RoleCheckError::Refused(RoleProblem::Unprotected(
            problems.join("; "),
        )))
    }
}

/// A failed role check.
#[derive(Debug, thiserror::Error)]
pub enum RoleCheckError {
    /// The role may not run the server.
    #[error(transparent)]
    Refused(RoleProblem),
    /// The check could not run.
    #[error(transparent)]
    Db(DbError),
}
