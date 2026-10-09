//! Users in PostgreSQL (`[database]`), managed through the admin API.
//!
//! `rustgit_users` holds one row per user with a salted password hash;
//! `rustgit_user_access` holds its access levels. Every lookup reads the
//! database, so a change applies to the next request on every server.
//! The schema is brought up to date when the server starts.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, PgConnection};

use super::{
    AuthError, Identity, IdentityProvider, NewUser, User, UserChange, UserError, UserSource,
    VerifiedHashes,
};
use crate::config::{Access, DatabaseConfig};

/// Schema changes, applied in order and recorded in `rustgit_schema`.
/// Never edit an applied entry; append a new one.
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE rustgit_users (
    name text PRIMARY KEY,
    password_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text NOT NULL
);
CREATE TABLE rustgit_user_access (
    user_name text NOT NULL REFERENCES rustgit_users (name) ON DELETE CASCADE,
    -- A repository name or '*'.
    repository text NOT NULL,
    level text NOT NULL CHECK (level IN ('none', 'read', 'write', 'force', 'admin')),
    PRIMARY KEY (user_name, repository)
);
CREATE INDEX rustgit_user_access_repository ON rustgit_user_access (repository);
"#];

/// Serializes schema changes between servers that start together.
const MIGRATION_LOCK: i64 = 0x7275_7374_6769_7401;

/// Users with their access levels; `$1` NULL for all users.
const SELECT_USERS: &str = "
SELECT u.name,
       (extract(epoch FROM u.created_at) * 1000)::bigint,
       (extract(epoch FROM u.updated_at) * 1000)::bigint,
       u.updated_by,
       a.repository,
       a.level
FROM rustgit_users u
LEFT JOIN rustgit_user_access a ON a.user_name = u.name
WHERE $1::text IS NULL OR u.name = $1
ORDER BY u.name";

type UserRow = (String, i64, i64, String, Option<String>, Option<String>);

pub struct PgUsers {
    pool: PgPool,
    verified: VerifiedHashes,
}

impl PgUsers {
    /// Connects and applies missing schema changes.
    pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<Self> {
        let options: PgConnectOptions = cfg
            .url
            .parse()
            .map_err(|e| anyhow::anyhow!("database.url: {e}"))?;
        let pool = PgPoolOptions::new()
            .max_connections(cfg.max_connections)
            .acquire_timeout(Duration::from_secs(cfg.timeout_secs))
            .connect_with(options)
            .await
            .map_err(|e| anyhow::anyhow!("database: {e}"))?;
        migrate(&pool)
            .await
            .map_err(|e| anyhow::anyhow!("database schema: {e}"))?;
        Ok(Self {
            pool,
            verified: VerifiedHashes::default(),
        })
    }

    async fn conn(&self) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, UserError> {
        self.pool.acquire().await.map_err(unavailable)
    }
}

async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let mut tx = conn.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS rustgit_schema (
             version integer PRIMARY KEY,
             applied_at timestamptz NOT NULL DEFAULT now()
         )",
    )
    .execute(&mut *tx)
    .await?;
    let current: i32 = sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM rustgit_schema")
        .fetch_one(&mut *tx)
        .await?;
    let known = MIGRATIONS.len() as i32;
    anyhow::ensure!(
        current <= known,
        "the database has schema version {current}; this program knows up to {known}"
    );
    for (version, &sql) in (1..).zip(MIGRATIONS).skip(current as usize) {
        sqlx::raw_sql(sql).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO rustgit_schema (version) VALUES ($1)")
            .bind(version)
            .execute(&mut *tx)
            .await?;
        tracing::info!(version, "applied database schema change");
    }
    tx.commit().await?;
    Ok(())
}

#[async_trait]
impl IdentityProvider for PgUsers {
    async fn authenticate(
        &self,
        user: &str,
        password: &str,
    ) -> Result<Option<Identity>, AuthError> {
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT u.password_hash, a.repository, a.level
             FROM rustgit_users u
             LEFT JOIN rustgit_user_access a ON a.user_name = u.name
             WHERE u.name = $1",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
        let Some((hash, _, _)) = rows.first() else {
            return Ok(None);
        };
        if !self.verified.verify(user, password, hash).await {
            return Err(AuthError::BadCredentials);
        }
        let mut access = BTreeMap::new();
        for (_, repo, level) in rows {
            if let (Some(repo), Some(level)) = (repo, level) {
                let level = parse_level(&level)
                    .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
                access.insert(repo, level);
            }
        }
        Ok(Some(Identity::User {
            name: user.to_string(),
            access,
        }))
    }

    fn manages_users(&self) -> bool {
        true
    }

    async fn list(&self) -> Result<Vec<User>, UserError> {
        users(&mut *self.conn().await?, None).await
    }

    async fn create(&self, new: NewUser, by: &str) -> Result<User, UserError> {
        let mut conn = self.conn().await?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        let inserted = sqlx::query(
            "INSERT INTO rustgit_users (name, password_hash, updated_by)
             VALUES ($1, $2, $3)
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(&new.name)
        .bind(&new.password_hash)
        .bind(by)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        if inserted.rows_affected() == 0 {
            return Err(UserError::Exists(new.name));
        }
        set_access(&mut tx, &new.name, &new.access).await?;
        let user = one(&mut tx, &new.name).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(user)
    }

    async fn update(&self, name: &str, change: UserChange, by: &str) -> Result<User, UserError> {
        let mut conn = self.conn().await?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        // Also locks the row until the end of the change.
        let updated = sqlx::query(
            "UPDATE rustgit_users
             SET password_hash = coalesce($2, password_hash),
                 updated_at = now(),
                 updated_by = $3
             WHERE name = $1",
        )
        .bind(name)
        .bind(&change.password_hash)
        .bind(by)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        if updated.rows_affected() == 0 {
            return Err(UserError::NotFound(name.into()));
        }
        if let Some(access) = &change.access {
            sqlx::query("DELETE FROM rustgit_user_access WHERE user_name = $1")
                .bind(name)
                .execute(&mut *tx)
                .await
                .map_err(unavailable)?;
            set_access(&mut tx, name, access).await?;
        }
        let user = one(&mut tx, name).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(user)
    }

    async fn delete(&self, name: &str, _by: &str) -> Result<(), UserError> {
        let deleted = sqlx::query("DELETE FROM rustgit_users WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        if deleted.rows_affected() == 0 {
            return Err(UserError::NotFound(name.into()));
        }
        Ok(())
    }
}

async fn set_access(
    conn: &mut PgConnection,
    name: &str,
    access: &BTreeMap<String, Access>,
) -> Result<(), UserError> {
    let (repos, levels): (Vec<&str>, Vec<&str>) = access
        .iter()
        .map(|(r, l)| (r.as_str(), level_name(*l)))
        .unzip();
    sqlx::query(
        "INSERT INTO rustgit_user_access (user_name, repository, level)
         SELECT $1, r, l FROM unnest($2::text[], $3::text[]) AS t (r, l)",
    )
    .bind(name)
    .bind(repos)
    .bind(levels)
    .execute(conn)
    .await
    .map_err(unavailable)?;
    Ok(())
}

async fn one(conn: &mut PgConnection, name: &str) -> Result<User, UserError> {
    users(conn, Some(name))
        .await?
        .pop()
        .ok_or_else(|| UserError::NotFound(name.into()))
}

async fn users(conn: &mut PgConnection, name: Option<&str>) -> Result<Vec<User>, UserError> {
    let rows: Vec<UserRow> = sqlx::query_as(SELECT_USERS)
        .bind(name)
        .fetch_all(conn)
        .await
        .map_err(unavailable)?;
    let mut out: Vec<User> = Vec::new();
    for (name, created_ms, updated_ms, updated_by, repo, level) in rows {
        if out.last().is_none_or(|u| u.name != name) {
            out.push(User {
                name,
                access: BTreeMap::new(),
                source: UserSource::Database,
                created_ms: Some(created_ms as u64),
                updated_ms: Some(updated_ms as u64),
                updated_by: Some(updated_by),
            });
        }
        if let (Some(repo), Some(level)) = (repo, level) {
            let level = parse_level(&level).map_err(UserError::Unavailable)?;
            out.last_mut().expect("pushed").access.insert(repo, level);
        }
    }
    Ok(out)
}

fn unavailable(e: sqlx::Error) -> UserError {
    UserError::Unavailable(e.to_string())
}

fn level_name(a: Access) -> &'static str {
    match a {
        Access::None => "none",
        Access::Read => "read",
        Access::Write => "write",
        Access::Force => "force",
        Access::Admin => "admin",
    }
}

fn parse_level(s: &str) -> Result<Access, String> {
    Ok(match s {
        "none" => Access::None,
        "read" => Access::Read,
        "write" => Access::Write,
        "force" => Access::Force,
        "admin" => Access::Admin,
        _ => return Err(format!("unknown access level {s:?}")),
    })
}
