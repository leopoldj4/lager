//! Activity of users for their profiles in PostgreSQL
//! (docs/decisions/0029-activity-graph.md).
//!
//! The server reads the commits of each repository's default branch from
//! the last year and keeps their author email and day here, up to the
//! commit it read last. A user's activity on a day is the commits with one
//! of the user's email addresses in the repositories the user can read,
//! each commit counted once, plus the pull requests the user opened here.
//! The daily counts of the user's GitHub account are kept beside them.

use serde::Serialize;
use sqlx::postgres::PgPool;

use crate::model::RepoId;

/// Days of history that the server reads and profiles show: 53 weeks and
/// a few days to spare for time zones.
pub const DAYS: u32 = 375;

#[derive(Debug, thiserror::Error)]
pub enum ActivityError {
    #[error("database unavailable: {0}")]
    Unavailable(String),
}

fn unavailable(e: sqlx::Error) -> ActivityError {
    ActivityError::Unavailable(e.to_string())
}

/// Where the commits of a repository were read up to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indexed {
    /// The default branch's full ref name.
    pub branch: String,
    /// Its commit; empty for an empty repository.
    pub tip: String,
}

/// A commit on a default branch.
pub struct CommitDay {
    pub oid: String,
    /// In lower case.
    pub email: String,
    /// `YYYY-MM-DD` in the author's time zone.
    pub day: String,
}

/// A day with activity.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Day {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub count: u32,
}

#[derive(Clone)]
pub struct ActivityDb {
    pool: PgPool,
}

impl ActivityDb {
    /// The schema is applied by [`crate::identity::PgUsers::connect`].
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn indexed(&self, repo: RepoId) -> Result<Option<Indexed>, ActivityError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT branch, tip FROM lager_activity_repos WHERE repository = $1")
                .bind(repo.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(unavailable)?;
        Ok(row.map(|(branch, tip)| Indexed { branch, tip }))
    }

    /// The repositories whose commits were read at least once.
    pub async fn indexed_repos(&self) -> Result<Vec<String>, ActivityError> {
        sqlx::query_scalar("SELECT repository FROM lager_activity_repos")
            .fetch_all(&self.pool)
            .await
            .map_err(unavailable)
    }

    /// Records the commits read from `from` (`None`: from the start) up to
    /// `to`. Changes nothing and answers `false` if another server moved the
    /// repository past `from` meanwhile. Reading from the start replaces
    /// the repository's commits; days before the last [`DAYS`] go.
    pub async fn record(
        &self,
        repo: RepoId,
        from: Option<&Indexed>,
        to: &Indexed,
        commits: &[CommitDay],
    ) -> Result<bool, ActivityError> {
        let id = repo.to_string();
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('lager_activity:' || $1))")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let now: Option<(String, String)> =
            sqlx::query_as("SELECT branch, tip FROM lager_activity_repos WHERE repository = $1")
                .bind(&id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(unavailable)?;
        let now = now.map(|(branch, tip)| Indexed { branch, tip });
        if from.is_some() && now.as_ref() != from {
            return Ok(false);
        }
        if now.as_ref() == Some(to) {
            return Ok(true);
        }
        if from.is_none() {
            sqlx::query("DELETE FROM lager_activity_commits WHERE repository = $1")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(unavailable)?;
        }
        for chunk in commits.chunks(5000) {
            let oids: Vec<&str> = chunk.iter().map(|c| c.oid.as_str()).collect();
            let emails: Vec<&str> = chunk.iter().map(|c| c.email.as_str()).collect();
            let days: Vec<&str> = chunk.iter().map(|c| c.day.as_str()).collect();
            sqlx::query(
                "INSERT INTO lager_activity_commits (repository, oid, email, day)
                 SELECT $1, o, e, d::date FROM unnest($2::text[], $3::text[], $4::text[]) AS t(o, e, d)
                 ON CONFLICT DO NOTHING",
            )
            .bind(&id)
            .bind(&oids)
            .bind(&emails)
            .bind(&days)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        }
        sqlx::query(
            "DELETE FROM lager_activity_commits
             WHERE repository = $1 AND day < current_date - $2::int",
        )
        .bind(&id)
        .bind(DAYS as i32)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        sqlx::query(
            "INSERT INTO lager_activity_repos (repository, branch, tip)
             VALUES ($1, $2, $3)
             ON CONFLICT (repository) DO UPDATE
             SET branch = $2, tip = $3, indexed_at = now()",
        )
        .bind(&id)
        .bind(&to.branch)
        .bind(&to.tip)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(true)
    }

    /// Forgets a deleted repository.
    pub async fn forget(&self, repo: RepoId) -> Result<(), ActivityError> {
        let id = repo.to_string();
        sqlx::query("DELETE FROM lager_activity_commits WHERE repository = $1")
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        sqlx::query("DELETE FROM lager_activity_repos WHERE repository = $1")
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// The user's commits and pull requests in `repos` on each of the last
    /// [`DAYS`] days, oldest first. A commit in several repositories counts
    /// once.
    pub async fn days(&self, user: &str, repos: &[RepoId]) -> Result<Vec<Day>, ActivityError> {
        let ids: Vec<String> = repos.iter().map(|r| r.to_string()).collect();
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT day::text, sum(n)::bigint FROM (
                 SELECT day, count(DISTINCT oid) AS n
                 FROM lager_activity_commits
                 WHERE repository = ANY($2)
                   AND email IN (SELECT email FROM lager_user_emails WHERE user_name = $1)
                   AND day >= current_date - $3::int
                 GROUP BY day
                 UNION ALL
                 SELECT created_at::date, count(*)
                 FROM lager_pulls
                 WHERE author = $1 AND github_url IS NULL AND repository = ANY($2)
                   AND created_at >= current_date - $3::int
                 GROUP BY created_at::date
             ) t
             GROUP BY day ORDER BY day",
        )
        .bind(user)
        .bind(&ids)
        .bind(DAYS as i32)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        Ok(rows
            .into_iter()
            .map(|(date, count)| Day {
                date,
                count: count as u32,
            })
            .collect())
    }
}
