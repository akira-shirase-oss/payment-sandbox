//! Database roles (review finding 1; D-038). The server never connects as a superuser or
//! as the owner of the tables: either could switch off the journal's triggers
//! (`session_replication_role`, `DISABLE TRIGGER`) or truncate it.
//!
//! - `ps_owner` owns the database and every object in it; only migrations use it.
//! - `ps_runtime` is a group role with exactly the grants the server needs
//!   (`migrations/0005_runtime_role.sql`): no `TRUNCATE`, no DDL, no `TEMP`.
//! - `ps_app` is the server's login, a member of `ps_runtime`, `NOSUPERUSER NOBYPASSRLS`.
//!
//! The passwords below are public development values for the local and CI stacks (D-023);
//! a deployed host chooses its own ([`RolePasswords`], D-071), and `setup` never resets them.

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

/// Passwords for the two login roles. `None` keeps an existing role's password and gives a
/// new role the public development one (the local and CI stacks, D-023).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RolePasswords {
    /// `ps_owner`'s.
    pub owner: Option<String>,
    /// `ps_app`'s.
    pub app: Option<String>,
}

/// A password refused for a database role.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "a role password is 16-128 characters of letters, digits and . _ ~ - (generate one with: openssl rand -hex 24)"
)]
pub struct WeakPassword;

impl RolePasswords {
    /// Checks both passwords given (letters, digits and `._~-` only, 16 to 128 of them, so
    /// they need no quoting anywhere).
    pub fn new(owner: Option<String>, app: Option<String>) -> Result<Self, WeakPassword> {
        let ok = |p: &Option<String>| {
            p.as_ref().is_none_or(|p| {
                (16..=128).contains(&p.len())
                    && p.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-')
                    })
            })
        };
        if ok(&owner) && ok(&app) {
            Ok(Self { owner, app })
        } else {
            Err(WeakPassword)
        }
    }
}

/// The statements [`bootstrap_with`] runs: each role is created if missing (with the given
/// password, or the development one), and an existing role's password changes only when one
/// is given. Earlier releases reset both passwords to the development values on every
/// `setup`, so re-running it on a deployed host (a restore, say) opened the database to the
/// public development credentials (D-071).
pub fn bootstrap_sql(passwords: &RolePasswords) -> String {
    let owner = passwords.owner.as_deref().unwrap_or(OWNER_PASSWORD);
    let app = passwords.app.as_deref().unwrap_or(APP_PASSWORD);
    let mut sql = format!(
        "BEGIN;
         SELECT pg_advisory_xact_lock(4242001);
         DO $$ BEGIN
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{OWNER}') THEN
             CREATE ROLE {OWNER} LOGIN NOSUPERUSER NOCREATEROLE NOBYPASSRLS PASSWORD '{owner}';
           END IF;
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{RUNTIME}') THEN
             CREATE ROLE {RUNTIME} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;
           END IF;
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{APP}') THEN
             CREATE ROLE {APP} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS IN ROLE {RUNTIME} PASSWORD '{app}';
           END IF;
         END $$;"
    );
    for (role, password) in [(OWNER, &passwords.owner), (APP, &passwords.app)] {
        if let Some(password) = password {
            sql.push_str("\n         ALTER ROLE ");
            sql.push_str(role);
            sql.push_str(" PASSWORD '");
            sql.push_str(password);
            sql.push_str("';");
        }
    }
    sql.push_str("\n         COMMIT;");
    sql
}

/// Creates the three roles if missing, with the development passwords for new ones (the
/// local and CI stacks and the tests). Needs a role with `CREATEROLE` (the stack's
/// superuser). Serialized by a transaction-scoped advisory lock (held until commit), so
/// parallel tests may call it.
pub async fn bootstrap(admin: &mut PgConnection) -> Result<(), DbError> {
    bootstrap_with(admin, &RolePasswords::default()).await
}

/// [`bootstrap`] with the passwords an operator chose (`PS_OWNER_PASSWORD`,
/// `PS_APP_PASSWORD` for `sandbox-server setup`).
pub async fn bootstrap_with(
    admin: &mut PgConnection,
    passwords: &RolePasswords,
) -> Result<(), DbError> {
    admin
        .execute(AssertSqlSafe(bootstrap_sql(passwords)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_never_resets_an_existing_roles_password_unless_one_is_given() {
        let dev = bootstrap_sql(&RolePasswords::default());
        assert!(!dev.contains("ALTER ROLE"), "{dev}");
        assert!(dev.contains(&format!("PASSWORD '{OWNER_PASSWORD}'")));
        let chosen = RolePasswords::new(
            Some("owner-0123456789abcdef".to_owned()),
            Some("app-0123456789abcdef".to_owned()),
        )
        .unwrap();
        let sql = bootstrap_sql(&chosen);
        assert!(sql.contains("ALTER ROLE ps_owner PASSWORD 'owner-0123456789abcdef'"));
        assert!(sql.contains("ALTER ROLE ps_app PASSWORD 'app-0123456789abcdef'"));
        assert!(!sql.contains(OWNER_PASSWORD) && !sql.contains(APP_PASSWORD));
    }

    #[test]
    fn role_passwords_are_long_and_need_no_quoting() {
        for weak in [
            "short",
            "has a space here!!",
            "quote'0123456789abcdef",
            "back\\slash0123456789",
        ] {
            assert_eq!(
                RolePasswords::new(Some(weak.to_owned()), None),
                Err(WeakPassword),
                "{weak}"
            );
        }
        assert!(RolePasswords::new(None, Some("0123456789abcdef0123".to_owned())).is_ok());
    }
}
