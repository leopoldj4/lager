//! Pull requests in PostgreSQL (docs/decisions/0019-pull-requests.md).
//!
//! The database holds what Git does not: titles, descriptions, states,
//! comments, and the commits the server last saw. Refs and merges are log
//! entries in storage. Every change raises the pull request's `version`,
//! which the live connection sends to watching pages.
//!
//! Changes that follow the repository (new commits, a merge) are
//! conditional on the recorded commits, so servers that notice the same push
//! record it once.

use serde::Serialize;
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{Connection, PgConnection, Row};

use crate::model::RepoId;

/// Longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 256;
/// Longest description or comment, in characters.
pub const MAX_BODY_CHARS: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PullState {
    Open,
    Closed,
    Merged,
}

impl PullState {
    fn parse(s: &str) -> Result<Self, PullError> {
        match s {
            "open" => Ok(Self::Open),
            "closed" => Ok(Self::Closed),
            "merged" => Ok(Self::Merged),
            _ => Err(PullError::Unavailable(format!("unknown state {s:?}"))),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Pull {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: String,
    /// Branch short names.
    pub source: String,
    pub target: String,
    pub state: PullState,
    /// The source commit the server last recorded. Pages of open pull
    /// requests show the source branch itself.
    pub head_oid: String,
    /// The target commit the server last recorded; the merge commit after a
    /// merge.
    pub target_oid: String,
    /// Where the diff starts: the merge base of the recorded commits, kept
    /// from before the merge.
    pub merge_base: String,
    pub merge_oid: Option<String>,
    /// `None` for a merge by a push.
    pub merged_by: Option<String>,
    pub version: u64,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub closed_ms: Option<u64>,
    #[serde(skip)]
    pub merge_request: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Comment,
    /// The source branch moved.
    Push,
    Closed,
    Reopened,
    Merged,
}

impl EventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Push => "push",
            Self::Closed => "closed",
            Self::Reopened => "reopened",
            Self::Merged => "merged",
        }
    }

    fn parse(s: &str) -> Result<Self, PullError> {
        Ok(match s {
            "comment" => Self::Comment,
            "push" => Self::Push,
            "closed" => Self::Closed,
            "reopened" => Self::Reopened,
            "merged" => Self::Merged,
            _ => return Err(PullError::Unavailable(format!("unknown event {s:?}"))),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PullEvent {
    pub id: i64,
    pub kind: EventKind,
    /// `None` for the server.
    pub author: Option<String>,
    pub body: Option<String>,
    pub old_oid: Option<String>,
    pub oid: Option<String>,
    pub created_ms: u64,
}

pub struct NewPull {
    pub title: String,
    pub body: String,
    pub author: String,
    pub source: String,
    pub target: String,
    pub head_oid: String,
    pub target_oid: String,
    pub merge_base: String,
    /// The client's request UUID: a retry answers the same pull request.
    pub request: Option<String>,
}

/// The commits of an open pull request, as the server saw them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commits {
    pub head: String,
    pub target: String,
    pub merge_base: String,
}

impl Pull {
    pub fn commits(&self) -> Commits {
        Commits {
            head: self.head_oid.clone(),
            target: self.target_oid.clone(),
            merge_base: self.merge_base.clone(),
        }
    }
}

/// Open and closed (with merged) pull requests of a repository.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Counts {
    pub open: u64,
    pub closed: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum PullError {
    #[error("pull request not found")]
    NotFound,
    /// An open pull request has the same branches.
    #[error("pull request #{0} is open for these branches")]
    Exists(u64),
    /// The pull request is not in the state the change needs.
    #[error("{0}")]
    State(String),
    #[error("{0}")]
    Invalid(String),
    #[error("database unavailable: {0}")]
    Unavailable(String),
}

fn unavailable(e: sqlx::Error) -> PullError {
    PullError::Unavailable(e.to_string())
}

/// The columns that [`pull`] reads. A macro, because queries take only constant
/// text.
macro_rules! pull_columns {
    () => {
        "number, title, body, author, source, target, state, head_oid, target_oid,
         merge_base, merge_oid, merged_by, merge_request, version,
         (extract(epoch FROM created_at) * 1000)::bigint,
         (extract(epoch FROM updated_at) * 1000)::bigint,
         (extract(epoch FROM closed_at) * 1000)::bigint"
    };
}

fn pull(row: PgRow) -> Result<Pull, PullError> {
    let get = |e: sqlx::Error| PullError::Unavailable(format!("database: {e}"));
    let state: String = row.try_get(6).map_err(get)?;
    let int = |i: usize| row.try_get::<i64, _>(i).map_err(get);
    Ok(Pull {
        number: int(0)? as u64,
        title: row.try_get(1).map_err(get)?,
        body: row.try_get(2).map_err(get)?,
        author: row.try_get(3).map_err(get)?,
        source: row.try_get(4).map_err(get)?,
        target: row.try_get(5).map_err(get)?,
        state: PullState::parse(&state)?,
        head_oid: row.try_get(7).map_err(get)?,
        target_oid: row.try_get(8).map_err(get)?,
        merge_base: row.try_get(9).map_err(get)?,
        merge_oid: row.try_get(10).map_err(get)?,
        merged_by: row.try_get(11).map_err(get)?,
        merge_request: row.try_get(12).map_err(get)?,
        version: int(13)? as u64,
        created_ms: int(14)? as u64,
        updated_ms: int(15)? as u64,
        closed_ms: row
            .try_get::<Option<i64>, _>(16)
            .map_err(get)?
            .map(|v| v as u64),
    })
}

type EventRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);

/// Checks a title and a description or comment.
pub fn validate_text(title: Option<&str>, body: &str) -> Result<(), PullError> {
    if let Some(t) = title {
        let n = t.trim().chars().count();
        if n == 0 || n > MAX_TITLE_CHARS || t.chars().any(char::is_control) {
            return Err(PullError::Invalid(format!(
                "titles need 1 to {MAX_TITLE_CHARS} characters and no control characters"
            )));
        }
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(PullError::Invalid(format!(
            "text longer than {MAX_BODY_CHARS} characters"
        )));
    }
    Ok(())
}

#[derive(Clone)]
pub struct Pulls {
    pool: PgPool,
}

impl Pulls {
    /// The schema is applied by [`crate::identity::PgUsers::connect`].
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Opens a pull request with the next number of the repository.
    pub async fn create(&self, repo: RepoId, new: NewPull) -> Result<Pull, PullError> {
        validate_text(Some(&new.title), &new.body)?;
        let repo = repo.to_string();
        let mut conn = self.pool.acquire().await.map_err(unavailable)?;
        if let Some(r) = &new.request
            && let Some(p) = by_request(&mut conn, &repo, r).await?
        {
            return Ok(p);
        }
        let mut tx = conn.begin().await.map_err(unavailable)?;
        let open: Option<i64> = sqlx::query_scalar(
            "SELECT number FROM lager_pulls
             WHERE repository = $1 AND source = $2 AND target = $3 AND state = 'open'",
        )
        .bind(&repo)
        .bind(&new.source)
        .bind(&new.target)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        if let Some(n) = open {
            return Err(PullError::Exists(n as u64));
        }
        let number: i64 = sqlx::query_scalar(
            "INSERT INTO lager_pull_numbers (repository, last) VALUES ($1, 1)
             ON CONFLICT (repository) DO UPDATE SET last = lager_pull_numbers.last + 1
             RETURNING last",
        )
        .bind(&repo)
        .fetch_one(&mut *tx)
        .await
        .map_err(unavailable)?;
        let inserted = sqlx::query(
            "INSERT INTO lager_pulls
                 (repository, number, title, body, author, source, target, state,
                  head_oid, target_oid, merge_base, request)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'open', $8, $9, $10, $11)",
        )
        .bind(&repo)
        .bind(number)
        .bind(new.title.trim())
        .bind(&new.body)
        .bind(&new.author)
        .bind(&new.source)
        .bind(&new.target)
        .bind(&new.head_oid)
        .bind(&new.target_oid)
        .bind(&new.merge_base)
        .bind(&new.request)
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {}
            // Another request opened one for the same branches, or this
            // request ran twice at the same time.
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                drop(tx);
                if let Some(r) = &new.request
                    && let Some(p) = by_request(&mut conn, &repo, r).await?
                {
                    return Ok(p);
                }
                return Err(PullError::State(
                    "a pull request for these branches was opened at the same time".into(),
                ));
            }
            Err(e) => return Err(unavailable(e)),
        }
        let p = one(&mut tx, &repo, number).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(p)
    }

    pub async fn get(&self, repo: RepoId, number: u64) -> Result<Option<Pull>, PullError> {
        let mut conn = self.pool.acquire().await.map_err(unavailable)?;
        find(&mut conn, &repo.to_string(), number as i64).await
    }

    /// Newest first. `open`: open ones, else closed and merged ones.
    pub async fn list(
        &self,
        repo: RepoId,
        open: bool,
        skip: u32,
        limit: u32,
    ) -> Result<(Vec<Pull>, Counts), PullError> {
        let repo = repo.to_string();
        let rows = sqlx::query(concat!(
            "SELECT ",
            pull_columns!(),
            " FROM lager_pulls
             WHERE repository = $1 AND (state = 'open') = $2
             ORDER BY number DESC OFFSET $3 LIMIT $4"
        ))
        .bind(&repo)
        .bind(open)
        .bind(skip as i64)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        let counts: Vec<(bool, i64)> = sqlx::query_as(
            "SELECT state = 'open', count(*) FROM lager_pulls
             WHERE repository = $1 GROUP BY 1",
        )
        .bind(&repo)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        let mut c = Counts::default();
        for (is_open, n) in counts {
            if is_open {
                c.open = n as u64;
            } else {
                c.closed += n as u64;
            }
        }
        Ok((rows.into_iter().map(pull).collect::<Result<_, _>>()?, c))
    }

    /// Unmerged pull requests, including closed ones with a pending merge receipt.
    pub async fn unmerged(&self, repo: RepoId) -> Result<Vec<Pull>, PullError> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            pull_columns!(),
            " FROM lager_pulls
             WHERE repository = $1 AND state != 'merged' ORDER BY number"
        ))
        .bind(repo.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        rows.into_iter().map(pull).collect()
    }

    /// The timeline of a pull request, oldest first.
    pub async fn events(&self, repo: RepoId, number: u64) -> Result<Vec<PullEvent>, PullError> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "SELECT id, kind, author, body, old_oid, oid,
                    (extract(epoch FROM created_at) * 1000)::bigint
             FROM lager_pull_events
             WHERE repository = $1 AND number = $2 ORDER BY id",
        )
        .bind(repo.to_string())
        .bind(number as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(unavailable)?;
        rows.into_iter()
            .map(|(id, kind, author, body, old_oid, oid, at)| {
                Ok(PullEvent {
                    id,
                    kind: EventKind::parse(&kind)?,
                    author,
                    body,
                    old_oid,
                    oid,
                    created_ms: at as u64,
                })
            })
            .collect()
    }

    /// Adds a comment. A retry with the same request adds nothing.
    pub async fn comment(
        &self,
        repo: RepoId,
        number: u64,
        author: &str,
        body: &str,
        request: Option<&str>,
    ) -> Result<Pull, PullError> {
        if body.trim().is_empty() {
            return Err(PullError::Invalid("the comment is empty".into()));
        }
        validate_text(None, body)?;
        let repo = repo.to_string();
        let mut conn = self.pool.acquire().await.map_err(unavailable)?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        let bumped = sqlx::query(
            "UPDATE lager_pulls SET version = version + 1, updated_at = now()
             WHERE repository = $1 AND number = $2",
        )
        .bind(&repo)
        .bind(number as i64)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        if bumped.rows_affected() == 0 {
            return Err(PullError::NotFound);
        }
        let added = sqlx::query(
            "INSERT INTO lager_pull_events (repository, number, kind, author, body, request)
             VALUES ($1, $2, 'comment', $3, $4, $5)
             ON CONFLICT (request) DO NOTHING",
        )
        .bind(&repo)
        .bind(number as i64)
        .bind(author)
        .bind(body)
        .bind(request)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        if added.rows_affected() == 0 {
            // A retry: keep the version.
            drop(tx);
            return find(&mut conn, &repo, number as i64)
                .await?
                .ok_or(PullError::NotFound);
        }
        let p = one(&mut tx, &repo, number as i64).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(p)
    }

    /// Closes an open pull request, or opens a closed one again. Merged
    /// pull requests stay merged.
    pub async fn set_open(
        &self,
        repo: RepoId,
        number: u64,
        open: bool,
        by: &str,
    ) -> Result<Pull, PullError> {
        let repo = repo.to_string();
        let mut conn = self.pool.acquire().await.map_err(unavailable)?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        let (from, to, kind) = if open {
            ("closed", "open", EventKind::Reopened)
        } else {
            ("open", "closed", EventKind::Closed)
        };
        let changed = sqlx::query(
            "UPDATE lager_pulls
             SET state = $3, version = version + 1, updated_at = now(),
                 closed_at = CASE WHEN $3 = 'open' THEN NULL ELSE now() END
             WHERE repository = $1 AND number = $2 AND state = $4",
        )
        .bind(&repo)
        .bind(number as i64)
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await;
        match changed {
            Ok(r) if r.rows_affected() == 1 => {}
            Ok(_) => {
                drop(tx);
                let p = find(&mut conn, &repo, number as i64)
                    .await?
                    .ok_or(PullError::NotFound)?;
                return Err(PullError::State(match p.state {
                    PullState::Merged => "the pull request is merged".into(),
                    PullState::Open => "the pull request is open".into(),
                    PullState::Closed => "the pull request is closed".into(),
                }));
            }
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(PullError::State(
                    "another pull request is open for these branches".into(),
                ));
            }
            Err(e) => return Err(unavailable(e)),
        }
        event(&mut tx, &repo, number, kind, Some(by), None, None).await?;
        let p = one(&mut tx, &repo, number as i64).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(p)
    }

    /// Records the commits of an open pull request, if the recorded ones
    /// are still `old`. A new head adds a push to the timeline. `None`:
    /// nothing changed.
    pub async fn record_commits(
        &self,
        repo: RepoId,
        number: u64,
        old: &Commits,
        new: &Commits,
    ) -> Result<Option<Pull>, PullError> {
        let repo = repo.to_string();
        let mut conn = self.pool.acquire().await.map_err(unavailable)?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        let changed = sqlx::query(
            "UPDATE lager_pulls
             SET head_oid = $6, target_oid = $7, merge_base = $8,
                 version = version + 1, updated_at = now()
             WHERE repository = $1 AND number = $2 AND state = 'open'
               AND head_oid = $3 AND target_oid = $4 AND merge_base = $5",
        )
        .bind(&repo)
        .bind(number as i64)
        .bind(&old.head)
        .bind(&old.target)
        .bind(&old.merge_base)
        .bind(&new.head)
        .bind(&new.target)
        .bind(&new.merge_base)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        if changed.rows_affected() == 0 {
            return Ok(None);
        }
        if old.head != new.head {
            event(
                &mut tx,
                &repo,
                number,
                EventKind::Push,
                None,
                Some(&old.head),
                Some(&new.head),
            )
            .await?;
        }
        let p = one(&mut tx, &repo, number as i64).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(Some(p))
    }

    /// Marks a pull request merged into `merge_oid`. A server receipt is
    /// authoritative even if someone closed it, or a sync first attributed
    /// the merge to a push. `None`: no change is needed.
    #[allow(clippy::too_many_arguments)]
    pub async fn mark_merged(
        &self,
        repo: RepoId,
        number: u64,
        head: &str,
        merge_oid: &str,
        merge_base: &str,
        by: Option<&str>,
        request: Option<&str>,
    ) -> Result<Option<Pull>, PullError> {
        let repo = repo.to_string();
        let mut conn = self.pool.acquire().await.map_err(unavailable)?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        let old: Option<String> = sqlx::query_scalar(
            "SELECT head_oid FROM lager_pulls
             WHERE repository = $1 AND number = $2
               AND (state = 'open' OR ($3 AND (state = 'closed' OR merge_request IS NULL))) FOR UPDATE",
        )
        .bind(&repo)
        .bind(number as i64)
        .bind(by.is_some())
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        let Some(old) = old else { return Ok(None) };
        if old != head {
            event(
                &mut tx,
                &repo,
                number,
                EventKind::Push,
                None,
                Some(&old),
                Some(head),
            )
            .await?;
        }
        sqlx::query(
            "UPDATE lager_pulls
             SET state = 'merged', head_oid = $3, target_oid = $4, merge_oid = $4,
                 merge_base = $5, merged_by = $6, merge_request = $7,
                 version = version + 1, updated_at = now(), closed_at = now()
             WHERE repository = $1 AND number = $2",
        )
        .bind(&repo)
        .bind(number as i64)
        .bind(head)
        .bind(merge_oid)
        .bind(merge_base)
        .bind(by)
        .bind(request)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        event(
            &mut tx,
            &repo,
            number,
            EventKind::Merged,
            by,
            None,
            Some(merge_oid),
        )
        .await?;
        let p = one(&mut tx, &repo, number as i64).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(Some(p))
    }
}

async fn by_request(
    conn: &mut PgConnection,
    repo: &str,
    request: &str,
) -> Result<Option<Pull>, PullError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        pull_columns!(),
        " FROM lager_pulls WHERE repository = $1 AND request = $2"
    ))
    .bind(repo)
    .bind(request)
    .fetch_optional(conn)
    .await
    .map_err(unavailable)?;
    row.map(pull).transpose()
}

async fn find(conn: &mut PgConnection, repo: &str, number: i64) -> Result<Option<Pull>, PullError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        pull_columns!(),
        " FROM lager_pulls WHERE repository = $1 AND number = $2"
    ))
    .bind(repo)
    .bind(number)
    .fetch_optional(conn)
    .await
    .map_err(unavailable)?;
    row.map(pull).transpose()
}

async fn one(conn: &mut PgConnection, repo: &str, number: i64) -> Result<Pull, PullError> {
    find(conn, repo, number).await?.ok_or(PullError::NotFound)
}

async fn event(
    conn: &mut PgConnection,
    repo: &str,
    number: u64,
    kind: EventKind,
    author: Option<&str>,
    old_oid: Option<&str>,
    oid: Option<&str>,
) -> Result<(), PullError> {
    sqlx::query(
        "INSERT INTO lager_pull_events (repository, number, kind, author, old_oid, oid)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(repo)
    .bind(number as i64)
    .bind(kind.as_str())
    .bind(author)
    .bind(old_oid)
    .bind(oid)
    .execute(conn)
    .await
    .map_err(unavailable)?;
    Ok(())
}
