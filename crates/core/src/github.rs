//! GitHub accounts and read-only mirrors in PostgreSQL
//! (docs/decisions/0020-github-mirrors.md).
//!
//! A user connects one GitHub account; its OAuth token fetches the
//! repositories that the user mirrors. Each mirror row says when it is due.
//! A server that starts a sync moves `next_sync_at` ahead by a lease in the
//! same statement that picks the row, so two servers rarely sync the same
//! mirror; if they do, the head CAS orders their pushes.
//!
//! A mirror that a page shows is due every few seconds, and a webhook
//! delivery or a new watch pokes it: due at once, or again right after the
//! sync that runs (docs/decisions/0027-live-github-mirrors.md).

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
    /// The mirror's webhook on GitHub.
    pub hook_id: Option<i64>,
    /// No webhook, and the last attempt to add one is old enough to try
    /// again.
    pub hook_due: bool,
}

/// A mirror's webhook on GitHub, with what removing it needs.
pub struct Hook {
    pub github: String,
    pub id: i64,
    pub token: Option<String>,
}

/// Where the import of a mirror's pull requests and comments stands
/// (docs/decisions/0026-github-pull-requests.md).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullCursors {
    /// GitHub's `updated_at` of the newest pull request seen.
    pub pulls_since: Option<String>,
    pub pulls_etag: Option<String>,
    /// The next page of the first import, newest first; `None` once done.
    pub import_page: Option<u32>,
    /// GitHub's `updated_at` of the newest comment seen, and the page of
    /// comments with that time to read next.
    pub comments_since: Option<String>,
    pub comments_page: u32,
    pub comments_etag: Option<String>,
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
    pub async fn claim(
        &self,
        lease: Duration,
        hook_retry: Duration,
    ) -> Result<Option<Due>, GithubDbError> {
        type Row = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<i64>,
            bool,
        );
        let row: Option<Row> = sqlx::query_as(
            "UPDATE lager_github_mirrors m
             SET next_sync_at = now() + make_interval(secs => $1), claimed_at = now()
             WHERE m.repository = (
                 SELECT repository FROM lager_github_mirrors
                 WHERE next_sync_at <= now()
                 ORDER BY next_sync_at
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING m.repository, m.name, m.user_name, m.github,
                 (SELECT token FROM lager_github_accounts a WHERE a.user_name = m.user_name),
                 m.hook_id,
                 m.hook_id IS NULL AND (m.hook_checked_at IS NULL
                     OR m.hook_checked_at < now() - make_interval(secs => $2))",
        )
        .bind(lease.as_secs_f64())
        .bind(hook_retry.as_secs_f64())
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        let Some((repo, name, user, github, token, hook_id, hook_due)) = row else {
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
            hook_id,
            hook_due,
        }))
    }

    /// Asks for a sync now: at once, or right after the one that runs. A
    /// due time beyond `interval` is a running sync's lease. `false`: no
    /// such mirror.
    pub async fn poke(&self, repo: RepoId, interval: Duration) -> Result<bool, GithubDbError> {
        let r = sqlx::query(
            "UPDATE lager_github_mirrors
             SET poked_at = now(),
                 next_sync_at = CASE
                     WHEN next_sync_at <= now() + make_interval(secs => $2)
                     THEN least(next_sync_at, now()) ELSE next_sync_at END
             WHERE repository = $1",
        )
        .bind(repo.to_string())
        .bind(interval.as_secs_f64())
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(r.rows_affected() > 0)
    }

    /// Marks mirrors as shown by a page for `ttl`. Other repositories are
    /// left alone.
    pub async fn watch(&self, repos: &[RepoId], ttl: Duration) -> Result<(), GithubDbError> {
        if repos.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = repos.iter().map(|r| r.to_string()).collect();
        sqlx::query(
            "UPDATE lager_github_mirrors SET watched_until = now() + make_interval(secs => $2)
             WHERE repository = ANY($1)",
        )
        .bind(&ids)
        .bind(ttl.as_secs_f64())
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    /// The secret of the mirror's webhook.
    pub async fn hook_secret(&self, repo: RepoId) -> Result<Option<String>, GithubDbError> {
        let secret: Option<Option<String>> = sqlx::query_scalar(
            "SELECT hook_secret FROM lager_github_mirrors WHERE repository = $1",
        )
        .bind(repo.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(secret.flatten())
    }

    /// Records the mirror's webhook, or an attempt that GitHub refused.
    pub async fn set_hook(
        &self,
        repo: RepoId,
        hook: Option<(i64, &str)>,
    ) -> Result<(), GithubDbError> {
        sqlx::query(
            "UPDATE lager_github_mirrors
             SET hook_id = $2, hook_secret = $3, hook_checked_at = now()
             WHERE repository = $1",
        )
        .bind(repo.to_string())
        .bind(hook.map(|h| h.0))
        .bind(hook.map(|h| h.1))
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    /// The mirror's webhook, if it has one.
    pub async fn hook(&self, repo: RepoId) -> Result<Option<Hook>, GithubDbError> {
        let row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT m.github, m.hook_id,
                 (SELECT token FROM lager_github_accounts a WHERE a.user_name = m.user_name)
             FROM lager_github_mirrors m WHERE m.repository = $1",
        )
        .bind(repo.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(row.and_then(|(github, id, token)| id.map(|id| Hook { github, id, token })))
    }

    pub async fn cursors(&self, repo: RepoId) -> Result<Option<PullCursors>, GithubDbError> {
        type Row = (
            Option<String>,
            Option<String>,
            Option<i32>,
            Option<String>,
            i32,
            Option<String>,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT pulls_since, pulls_etag, pulls_import_page,
                    comments_since, comments_page, comments_etag
             FROM lager_github_mirrors WHERE repository = $1",
        )
        .bind(repo.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(row.map(
            |(pulls_since, pulls_etag, page, comments_since, comments_page, comments_etag)| {
                PullCursors {
                    pulls_since,
                    pulls_etag,
                    import_page: page.map(|p| p.max(1) as u32),
                    comments_since,
                    comments_page: comments_page.max(1) as u32,
                    comments_etag,
                }
            },
        ))
    }

    pub async fn set_cursors(&self, repo: RepoId, c: &PullCursors) -> Result<(), GithubDbError> {
        sqlx::query(
            "UPDATE lager_github_mirrors
             SET pulls_since = $2, pulls_etag = $3, pulls_import_page = $4,
                 comments_since = $5, comments_page = $6, comments_etag = $7
             WHERE repository = $1",
        )
        .bind(repo.to_string())
        .bind(&c.pulls_since)
        .bind(&c.pulls_etag)
        .bind(c.import_page.map(|p| p as i32))
        .bind(&c.comments_since)
        .bind(c.comments_page as i32)
        .bind(&c.comments_etag)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    /// Records the result of a sync. The next one is due at once if the
    /// mirror was poked during this one, after `watched` while a page
    /// watches it, else after `next`.
    pub async fn finish(
        &self,
        repo: RepoId,
        error: Option<&str>,
        next: Duration,
        watched: Duration,
    ) -> Result<(), GithubDbError> {
        sqlx::query(
            "UPDATE lager_github_mirrors
             SET next_sync_at = now() + CASE
                     WHEN poked_at >= claimed_at THEN interval '0'
                     WHEN watched_until > now() THEN make_interval(secs => $4)
                     ELSE make_interval(secs => $2) END,
                 synced_at = CASE WHEN $3::text IS NULL THEN now() ELSE synced_at END,
                 error = $3
             WHERE repository = $1",
        )
        .bind(repo.to_string())
        .bind(next.as_secs_f64())
        .bind(error)
        .bind(watched.as_secs_f64())
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(())
    }
}
