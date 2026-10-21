//! GitHub accounts and read-only mirrors in PostgreSQL
//! (docs/decisions/0020-github-mirrors.md).
//!
//! A user connects one GitHub account; its OAuth token fetches the
//! repositories that the user mirrors. Each mirror row says when it is due.
//! A server that starts a sync moves `next_sync_at` ahead by a lease in the
//! same statement that picks the row, so two servers rarely sync the same
//! mirror; if they do, the head CAS orders their pushes.

use std::time::Duration;

use serde::Serialize;
use sqlx::postgres::PgPool;

use crate::model::RepoId;

#[derive(Debug, thiserror::Error)]
pub enum GithubDbError {
    #[error("database unavailable: {0}")]
    Unavailable(String),
}

fn unavailable(e: sqlx::Error) -> GithubDbError {
    GithubDbError::Unavailable(e.to_string())
}

#[derive(Clone, Debug, Serialize)]
pub struct Account {
    pub login: String,
    #[serde(skip)]
    pub token: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Mirror {
    /// The repository's name here.
    pub name: String,
    /// owner/name on GitHub.
    pub github: String,
    pub synced_ms: Option<u64>,
    pub error: Option<String>,
}

/// A mirror that is due, with its user's token if the user is connected.
pub struct Due {
    pub repo: RepoId,
    pub name: String,
    pub user: String,
    pub github: String,
    pub token: Option<String>,
}

#[derive(Clone)]
pub struct GithubDb {
    pool: PgPool,
}

impl GithubDb {
    /// The schema is applied by [`crate::identity::PgUsers::connect`].
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn account(&self, user: &str) -> Result<Option<Account>, GithubDbError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT login, token FROM lager_github_accounts WHERE user_name = $1")
                .bind(user)
                .fetch_optional(&self.pool)
                .await
                .map_err(unavailable)?;
        Ok(row.map(|(login, token)| Account { login, token }))
    }

    /// Connects a GitHub account, replacing an earlier one. Mirrors that
    /// failed for want of a token are due at once.
    pub async fn connect(
        &self,
        user: &str,
        github_id: i64,
        login: &str,
        token: &str,
    ) -> Result<(), GithubDbError> {
        sqlx::query(
            "INSERT INTO lager_github_accounts (user_name, github_id, login, token)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (user_name) DO UPDATE
             SET github_id = $2, login = $3, token = $4, created_at = now()",
        )
        .bind(user)
        .bind(github_id)
        .bind(login)
        .bind(token)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        sqlx::query(
            "UPDATE lager_github_mirrors SET next_sync_at = now()
             WHERE user_name = $1 AND error IS NOT NULL",
        )
        .bind(user)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    /// Forgets the account. Its mirrors stay and stop syncing.
    pub async fn disconnect(&self, user: &str) -> Result<(), GithubDbError> {
        sqlx::query("DELETE FROM lager_github_accounts WHERE user_name = $1")
            .bind(user)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    pub async fn mirrors(&self, user: &str) -> Result<Vec<Mirror>, GithubDbError> {
        let rows: Vec<(String, String, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT name, github, (extract(epoch FROM synced_at) * 1000)::bigint, error
             FROM lager_github_mirrors WHERE user_name = $1 ORDER BY name",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(rows
            .into_iter()
            .map(|(name, github, synced, error)| Mirror {
                name,
                github,
                synced_ms: synced.map(|v| v as u64),
                error,
            })
            .collect())
    }

    /// Adds a mirror, due at once. A retry for the same repository keeps
    /// the row.
    pub async fn add(
        &self,
        repo: RepoId,
        name: &str,
        user: &str,
        github: &str,
    ) -> Result<(), GithubDbError> {
        sqlx::query(
            "INSERT INTO lager_github_mirrors (repository, name, user_name, github)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (repository) DO NOTHING",
        )
        .bind(repo.to_string())
        .bind(name)
        .bind(user)
        .bind(github)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    /// The user's mirror with this name, by repository.
    pub async fn find(&self, user: &str, name: &str) -> Result<Option<RepoId>, GithubDbError> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT repository FROM lager_github_mirrors WHERE user_name = $1 AND name = $2",
        )
        .bind(user)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(id.and_then(|i| i.parse::<uuid::Uuid>().ok()).map(RepoId))
    }

    pub async fn remove(&self, repo: RepoId) -> Result<(), GithubDbError> {
        sqlx::query("DELETE FROM lager_github_mirrors WHERE repository = $1")
            .bind(repo.to_string())
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Follows a rename of the mirror's repository.
    pub async fn rename(&self, repo: RepoId, name: &str) -> Result<(), GithubDbError> {
        sqlx::query("UPDATE lager_github_mirrors SET name = $2 WHERE repository = $1")
            .bind(repo.to_string())
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Takes the mirror that is due longest and leases it for `lease`.
    pub async fn claim(&self, lease: Duration) -> Result<Option<Due>, GithubDbError> {
        let row: Option<(String, String, String, String, Option<String>)> = sqlx::query_as(
            "UPDATE lager_github_mirrors m
             SET next_sync_at = now() + make_interval(secs => $1)
             WHERE m.repository = (
                 SELECT repository FROM lager_github_mirrors
                 WHERE next_sync_at <= now()
                 ORDER BY next_sync_at
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING m.repository, m.name, m.user_name, m.github,
                 (SELECT token FROM lager_github_accounts a WHERE a.user_name = m.user_name)",
        )
        .bind(lease.as_secs_f64())
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        let Some((repo, name, user, github, token)) = row else {
            return Ok(None);
        };
        let repo = repo
            .parse::<uuid::Uuid>()
            .map_err(|e| GithubDbError::Unavailable(format!("invalid repository id: {e}")))?;
        Ok(Some(Due {
            repo: RepoId(repo),
            name,
            user,
            github,
            token,
        }))
    }

    /// Records the result of a sync; the next one is due after `next`.
    pub async fn finish(
        &self,
        repo: RepoId,
        error: Option<&str>,
        next: Duration,
    ) -> Result<(), GithubDbError> {
        sqlx::query(
            "UPDATE lager_github_mirrors
             SET next_sync_at = now() + make_interval(secs => $2),
                 synced_at = CASE WHEN $3::text IS NULL THEN now() ELSE synced_at END,
                 error = $3
             WHERE repository = $1",
        )
        .bind(repo.to_string())
        .bind(next.as_secs_f64())
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(())
    }
}
