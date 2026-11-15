//! GitHub accounts and mirrors in PostgreSQL
//! (docs/decisions/0020-github-mirrors.md).
//!
//! A user connects one GitHub account; its OAuth token fetches the
//! repositories that the user mirrors, and sends the user's pushes and pull
//! request changes to GitHub
//! (docs/decisions/0028-two-way-github-mirrors.md). Each mirror row says when it is due.
//! A server that starts a sync moves `next_sync_at` ahead by a lease in the
//! same statement that picks the row, so two servers rarely sync the same
//! mirror; if they do, the head CAS orders their pushes.
//!
//! A mirror that a page shows is due every few seconds, and a webhook
//! delivery or a new watch pokes it: due at once, or again right after the
//! sync that runs (docs/decisions/0027-live-github-mirrors.md).
//!
//! An account can sync new repositories: GitHub repositories the user owns
//! that are created after the newest one seen become mirrors
//! (docs/decisions/0033-sync-new-github-repositories.md).

use std::time::Duration;

use serde::Serialize;
use sqlx::postgres::PgPool;
use ts_rs::TS;

use crate::activity::Day;
use crate::model::RepoId;

#[derive(Debug, thiserror::Error)]
pub enum GithubDbError {
    #[error("database unavailable: {0}")]
    Unavailable(String),
}

fn unavailable(e: sqlx::Error) -> GithubDbError {
    GithubDbError::Unavailable(e.to_string())
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "GithubAccount")]
pub struct Account {
    pub login: String,
    /// GitHub's user ID.
    #[serde(skip)]
    pub id: i64,
    #[serde(skip)]
    pub token: String,
    /// New repositories become mirrors.
    pub auto: bool,
    /// Why the last check failed.
    pub auto_error: Option<String>,
    /// The repositories that the last check to skip any skipped, and why.
    pub auto_skipped: Option<String>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "GithubMirror")]
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

/// An account that is due to look for new repositories.
pub struct AutoDue {
    pub user: String,
    pub token: String,
    /// GitHub's `created_at` of the newest repository seen.
    pub since: Option<String>,
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
        type Row = (String, i64, String, bool, Option<String>, Option<String>);
        let row: Option<Row> = sqlx::query_as(
            "SELECT login, github_id, token, auto, auto_error, auto_skipped
             FROM lager_github_accounts WHERE user_name = $1",
        )
        .bind(user)
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(row.map(
            |(login, id, token, auto, auto_error, auto_skipped)| Account {
                login,
                id,
                token,
                auto,
                auto_error,
                auto_skipped,
            },
        ))
    }

    /// Connects a GitHub account, replacing an earlier one. Mirrors that
    /// failed for want of a token are due at once. Another GitHub account
    /// does not sync new repositories until the user turns it on.
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
             SET github_id = $2, login = $3, token = $4, created_at = now(),
                 contributions_at = NULL, auto_error = NULL, auto_next_at = now(),
                 auto_skipped = CASE WHEN lager_github_accounts.github_id = $2
                     THEN lager_github_accounts.auto_skipped END,
                 auto = lager_github_accounts.auto AND lager_github_accounts.github_id = $2",
        )
        .bind(user)
        .bind(github_id)
        .bind(login)
        .bind(token)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        // Counts of an earlier account.
        sqlx::query("DELETE FROM lager_github_contributions WHERE user_name = $1")
            .bind(user)
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

    /// The daily counts of the user's GitHub account, oldest first, if the
    /// user is connected, and how long ago they were fetched; `None` for
    /// counts never fetched (docs/decisions/0029-activity-graph.md).
    pub async fn contributions(
        &self,
        user: &str,
    ) -> Result<Option<(Option<Duration>, Vec<Day>)>, GithubDbError> {
        let age: Option<Option<f64>> = sqlx::query_scalar(
            "SELECT extract(epoch FROM now() - contributions_at)::float8
             FROM lager_github_accounts WHERE user_name = $1",
        )
        .bind(user)
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        let Some(age) = age else {
            return Ok(None);
        };
        let rows: Vec<(String, i32)> = sqlx::query_as(
            "SELECT day::text, count FROM lager_github_contributions
             WHERE user_name = $1 AND day >= current_date - $2::int AND count > 0
             ORDER BY day",
        )
        .bind(user)
        .bind(crate::activity::DAYS as i32)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        let days = rows
            .into_iter()
            .map(|(date, count)| Day {
                date,
                count: count.max(0) as u32,
            })
            .collect();
        Ok(Some((
            age.map(|s| Duration::from_secs_f64(s.max(0.0))),
            days,
        )))
    }

    /// Replaces the daily counts of the user's GitHub account.
    pub async fn set_contributions(&self, user: &str, days: &[Day]) -> Result<(), GithubDbError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let updated = sqlx::query(
            "UPDATE lager_github_accounts SET contributions_at = now() WHERE user_name = $1",
        )
        .bind(user)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        if updated.rows_affected() == 0 {
            // Disconnected meanwhile.
            return Ok(());
        }
        sqlx::query("DELETE FROM lager_github_contributions WHERE user_name = $1")
            .bind(user)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let dates: Vec<&str> = days.iter().map(|d| d.date.as_str()).collect();
        let counts: Vec<i32> = days.iter().map(|d| d.count as i32).collect();
        sqlx::query(
            "INSERT INTO lager_github_contributions (user_name, day, count)
             SELECT $1, d::date, c FROM unnest($2::text[], $3::int[]) AS t(d, c)
             ON CONFLICT DO NOTHING",
        )
        .bind(user)
        .bind(&dates)
        .bind(&counts)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }

    // Sign-in with GitHub (docs/decisions/0031-github-sign-in.md).

    /// The user that the GitHub account `github_id` signs in, if any; keeps
    /// its current `login`.
    pub async fn signs_in(
        &self,
        github_id: i64,
        login: &str,
    ) -> Result<Option<String>, GithubDbError> {
        sqlx::query_scalar(
            "UPDATE lager_github_logins SET login = $2 WHERE github_id = $1 RETURNING user_name",
        )
        .bind(github_id)
        .bind(login)
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)
    }

    /// Lets the GitHub account sign the user in, unless either is linked
    /// already. Answers whether the user is linked to this account now.
    pub async fn link(
        &self,
        user: &str,
        github_id: i64,
        login: &str,
    ) -> Result<bool, GithubDbError> {
        sqlx::query(
            "INSERT INTO lager_github_logins (github_id, user_name, login)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(github_id)
        .bind(user)
        .bind(login)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(self
            .linked(user)
            .await?
            .is_some_and(|(id, _)| id == github_id))
    }

    /// Whether a connected account holds `token`.
    pub async fn token_in_use(&self, token: &str) -> Result<bool, GithubDbError> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lager_github_accounts WHERE token = $1)")
            .bind(token)
            .fetch_one(&self.pool)
            .await
            .map_err(unavailable)
    }

    /// The GitHub ID and login of the account that signs the user in.
    pub async fn linked(&self, user: &str) -> Result<Option<(i64, String)>, GithubDbError> {
        sqlx::query_as("SELECT github_id, login FROM lager_github_logins WHERE user_name = $1")
            .bind(user)
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)
    }

    /// Forgets the account and the email addresses it verified. Its
    /// mirrors stay and stop syncing. Sign-in with it stays.
    pub async fn disconnect(&self, user: &str) -> Result<(), GithubDbError> {
        sqlx::query("DELETE FROM lager_github_accounts WHERE user_name = $1")
            .bind(user)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        sqlx::query("DELETE FROM lager_user_emails WHERE user_name = $1 AND verified = 'github'")
            .bind(user)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Replaces the user's email addresses that GitHub verified, in lower
    /// case (docs/decisions/0029-activity-graph.md). An unverified address
    /// the user added by hand becomes GitHub's; one GitHub no longer lists
    /// goes. An address that another user verified stays theirs
    /// (docs/decisions/0032-email-verification.md).
    pub async fn set_emails(&self, user: &str, emails: &[String]) -> Result<(), GithubDbError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        sqlx::query(
            "DELETE FROM lager_user_emails
             WHERE user_name = $1 AND verified = 'github' AND NOT email = ANY($2)",
        )
        .bind(user)
        .bind(emails)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        sqlx::query(
            "INSERT INTO lager_user_emails (user_name, email, verified, verified_at)
             SELECT $1, e, 'github', now() FROM unnest($2::text[]) AS e
             WHERE EXISTS (SELECT 1 FROM lager_users WHERE name = $1)
               AND NOT EXISTS (
                   SELECT 1 FROM lager_user_emails o
                   WHERE o.email = e AND o.user_name <> $1 AND o.verified IS NOT NULL
               )
             ON CONFLICT (user_name, email) DO UPDATE
             SET verified = 'github', verified_at = now(), token_hash = NULL, token_expires_at = NULL
             WHERE lager_user_emails.verified IS NULL",
        )
        .bind(user)
        .bind(emails)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }

    /// Turns syncing new repositories on, with the newest repository seen
    /// so far, or off. `false`: not connected.
    pub async fn set_auto(
        &self,
        user: &str,
        on: bool,
        since: Option<&str>,
    ) -> Result<bool, GithubDbError> {
        let r = sqlx::query(
            "UPDATE lager_github_accounts
             SET auto = $2, auto_since = $3, auto_error = NULL, auto_skipped = NULL,
                 auto_next_at = now()
             WHERE user_name = $1",
        )
        .bind(user)
        .bind(on)
        .bind(since)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(r.rows_affected() > 0)
    }

    /// Takes the account that is due longest to look for new repositories
    /// and leases it for `lease`.
    pub async fn claim_auto(&self, lease: Duration) -> Result<Option<AutoDue>, GithubDbError> {
        let row: Option<(String, String, Option<String>)> = sqlx::query_as(
            "UPDATE lager_github_accounts a
             SET auto_next_at = now() + make_interval(secs => $1)
             WHERE a.user_name = (
                 SELECT user_name FROM lager_github_accounts
                 WHERE auto AND auto_next_at <= now()
                 ORDER BY auto_next_at
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING a.user_name, a.token, a.auto_since",
        )
        .bind(lease.as_secs_f64())
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(row.map(|(user, token, since)| AutoDue { user, token, since }))
    }

    /// Records a check for new repositories: the newest repository seen,
    /// why it failed, and the repositories it skipped, which replace the
    /// earlier ones. The next check is due after `next`. A check that
    /// started before the user turned syncing off or on again changes
    /// nothing.
    pub async fn finish_auto(
        &self,
        user: &str,
        started: Option<&str>,
        since: Option<&str>,
        error: Option<&str>,
        skipped: Option<&str>,
        next: Duration,
    ) -> Result<(), GithubDbError> {
        sqlx::query(
            "UPDATE lager_github_accounts
             SET auto_since = $3, auto_error = $4,
                 auto_skipped = coalesce($5, auto_skipped),
                 auto_next_at = now() + make_interval(secs => $6)
             WHERE user_name = $1 AND auto AND auto_since IS NOT DISTINCT FROM $2",
        )
        .bind(user)
        .bind(started)
        .bind(since)
        .bind(error)
        .bind(skipped)
        .bind(next.as_secs_f64())
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
