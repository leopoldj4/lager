//! The commits behind the activity on profiles
//! (docs/decisions/0029-activity-graph.md).
//!
//! After each change on this server, [`Activity::run`] reads the commits of
//! the repository's default branch since the commit it read last, back to
//! [`DAYS`] ago, and records their author email and day. A force push or a
//! new default branch reads the branch again from the start. At start the
//! server reads the repositories it never read, and forgets deleted ones.
//! Two servers may read the same change; the database keeps one result. A
//! lost event leaves a repository behind until its next change.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::{Semaphore, broadcast, watch};

use crate::bus::Event;
use crate::http::access::Directory;
use crate::repository::{Registry, RepoError};
use lager_core::activity::{ActivityDb, CommitDay, DAYS, Indexed};
use lager_core::model::{RepoId, RepoState};

/// Repositories read at the same time on one server.
const PARALLEL: usize = 2;
/// Commits read per change; a larger history keeps its newest commits.
const MAX_COMMITS: u32 = 100_000;
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Activity {
    pub db: ActivityDb,
    registry: Arc<Registry>,
    directory: Arc<Directory>,
    /// Repositories with a requested read that has not started.
    queued: StdMutex<HashSet<RepoId>>,
    permits: Semaphore,
}

impl Activity {
    pub fn new(db: ActivityDb, registry: Arc<Registry>, directory: Arc<Directory>) -> Self {
        Self {
            db,
            registry,
            directory,
            queued: StdMutex::new(HashSet::new()),
            permits: Semaphore::new(PARALLEL),
        }
    }

    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut events = self.registry.svc.bus.subscribe();
        self.clone().catch_up().await;
        loop {
            let event = tokio::select! {
                e = events.recv() => e,
                _ = crate::server::stopped(&mut stop) => break,
            };
            match event {
                Ok(Event::Changed(c)) if !c.remote => self.clone().request(c.repo),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "activity skipped change events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    /// Reads the repositories never read, and forgets deleted ones.
    async fn catch_up(self: Arc<Self>) {
        let (records, indexed) = match (
            self.directory.catalog().list().await,
            self.db.indexed_repos().await,
        ) {
            (Ok(r), Ok(i)) => (r, i),
            (Err(e), _) => {
                tracing::warn!(error = %e, "activity not caught up: catalog unavailable");
                return;
            }
            (_, Err(e)) => {
                tracing::warn!(error = %e, "activity not caught up");
                return;
            }
        };
        let active: HashSet<String> = records
            .iter()
            .filter(|r| r.state == RepoState::Active)
            .map(|r| r.repository.to_string())
            .collect();
        let indexed: HashSet<String> = indexed.into_iter().collect();
        for id in indexed.difference(&active) {
            if let Ok(id) = id.parse::<uuid::Uuid>() {
                let _ = self.db.forget(RepoId(id)).await;
            }
        }
        for r in records {
            if r.state == RepoState::Active && !indexed.contains(&r.repository.to_string()) {
                self.clone().request(r.repository);
            }
        }
    }

    /// Starts a read in the background, unless one waits to start already:
    /// that one sees the newer state too.
    pub fn request(self: Arc<Self>, repo: RepoId) {
        if !self.queued.lock().unwrap().insert(repo) {
            return;
        }
        tokio::spawn(async move {
            let _permit = self.permits.acquire().await;
            self.queued.lock().unwrap().remove(&repo);
            match self.index(repo).await {
                Ok(()) => {}
                Err(RepoError::NotFound | RepoError::Deleted) => {
                    let _ = self.db.forget(repo).await;
                }
                Err(e) => tracing::warn!(%repo, error = %e, "activity not read"),
            }
        });
    }

    async fn index(&self, repo: RepoId) -> Result<(), RepoError> {
        let view = self.registry.controller(repo).prepare_read().await?;
        let dir = &view.dir;
        let branch = self.git(dir, &["symbolic-ref", "-q", "HEAD"]).await?;
        let tip = self
            .git(dir, &["rev-parse", "-q", "--verify", "HEAD^{commit}"])
            .await?;
        let line = |out: Option<Vec<u8>>| {
            String::from_utf8_lossy(&out.unwrap_or_default())
                .trim()
                .to_string()
        };
        let now = Indexed {
            branch: line(branch),
            tip: line(tip),
        };
        let before = self.db.indexed(repo).await.map_err(unavailable)?;
        if before.as_ref() == Some(&now) {
            return Ok(());
        }
        // From where the last read ended, if the branch only moved ahead.
        let from = match &before {
            Some(b) if b.branch == now.branch && !b.tip.is_empty() && !now.tip.is_empty() => self
                .git(dir, &["merge-base", "--is-ancestor", &b.tip, &now.tip])
                .await?
                .map(|_| b),
            _ => None,
        };
        let commits = if now.tip.is_empty() {
            vec![]
        } else {
            let since = format!("--since={DAYS} days ago");
            let max = format!("--max-count={MAX_COMMITS}");
            let hide = from.map(|b| format!("^{}", b.tip));
            let mut args = vec![
                "rev-list",
                "--no-commit-header",
                "--format=%H %ad %ae",
                "--date=format:%Y-%m-%d",
                &since,
                &max,
                &now.tip,
            ];
            args.extend(hide.as_deref());
            let out = self.git(dir, &args).await?.unwrap_or_default();
            parse(&out)
        };
        let recorded = self
            .db
            .record(repo, from, &now, &commits)
            .await
            .map_err(unavailable)?;
        if recorded {
            tracing::debug!(%repo, commits = commits.len(), whole = from.is_none(), "activity read");
        }
        Ok(())
    }

    /// The output of a Git command in `dir`; `None` if it exits with 1, as
    /// Git's queries do for "no".
    async fn git(&self, dir: &Path, args: &[&str]) -> Result<Option<Vec<u8>>, RepoError> {
        let mut c = self.registry.svc.git.service_command();
        c.env("GIT_DIR", dir).args(args).kill_on_drop(true);
        let out = tokio::time::timeout(GIT_TIMEOUT, c.output())
            .await
            .map_err(|_| RepoError::Internal(format!("git {} took too long", args[0])))??;
        match out.status.code() {
            Some(0) => Ok(Some(out.stdout)),
            Some(1) => Ok(None),
            _ => Err(RepoError::Internal(format!(
                "git {} failed: {}",
                args[0],
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }
}

fn unavailable(e: lager_core::activity::ActivityError) -> RepoError {
    RepoError::Unavailable(e.to_string())
}

/// Lines of `<oid> <day> <email>`.
fn parse(out: &[u8]) -> Vec<CommitDay> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(|line| {
            let mut f = line.splitn(3, ' ');
            let (oid, day, email) = (f.next()?, f.next()?, f.next()?.trim());
            let valid_day = day.len() == 10 && day.bytes().all(|b| b.is_ascii_digit() || b == b'-');
            (valid_day && !email.is_empty()).then(|| CommitDay {
                oid: oid.to_string(),
                email: email.to_lowercase(),
                day: day.to_string(),
            })
        })
        .collect()
}
