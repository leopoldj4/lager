//! Pull request queries and changes of the web interface
//! (docs/decisions/0019-pull-requests.md).
//!
//! The database answers the pull requests and their timelines; a read view
//! answers their commits, so a page sees every push accepted before the
//! query. Changes go to [`PullService`](crate::pulls::PullService) and need
//! a session.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ops::{DEFAULT_LIMIT, Op, OpError, readable};
use super::read::{COMMIT_FORMAT, Commit, Git, MAX_DIFF_BYTES, open, parse_commits};
use crate::git::MergeTree;
use crate::github::Github;
use crate::http::AppInner;
use crate::pulls::PullOpError;
use lager_core::identity::Identity;
use lager_core::model::Oid;
use lager_core::pulls::{Counts, Pull, PullError, PullEvent, PullState};
use lager_core::repo::{Access, RepoSettings};

const MAX_PULLS: u32 = 100;
/// Commits shown for one comparison.
const MAX_COMMITS: usize = 250;

impl From<PullOpError> for OpError {
    fn from(e: PullOpError) -> Self {
        match e {
            PullOpError::NotFound => Self::not_found("pull request"),
            PullOpError::SignedOut => Self::new("unauthorized", "sign in first"),
            PullOpError::Forbidden(m) => Self::new("forbidden", m),
            PullOpError::Invalid(m) => Self::bad(m),
            PullOpError::State(m) => Self::new("invalid_state", m),
            PullOpError::Stale => Self::new("stale", PullOpError::Stale.to_string()),
            PullOpError::Conflict(files) => Self::new(
                "conflict",
                format!("the branches conflict in {}", files.join(", ")),
            ),
            PullOpError::Rejected(m) => Self::new("rejected", m),
            PullOpError::Busy(m) => Self::new("busy", m),
            PullOpError::Unavailable(m) => {
                tracing::warn!(error = %m, "pull requests unavailable");
                Self::new("unavailable", "pull requests unavailable; retry later")
            }
            PullOpError::Internal(m) => {
                tracing::error!(error = %m, "internal error");
                Self::new("internal", "internal error")
            }
        }
    }
}

impl From<PullError> for OpError {
    fn from(e: PullError) -> Self {
        PullOpError::from(e).into()
    }
}

/// A pull request after a change to it.
#[derive(Serialize, TS)]
pub struct PullChanged {
    pull: Pull,
}

#[derive(Serialize, TS)]
pub struct PullList {
    pulls: Vec<Pull>,
    counts: Counts,
    more: bool,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Mergeable {
    Clean,
    Conflict,
    UpToDate,
    SourceMissing,
    TargetMissing,
}

#[derive(Serialize, TS)]
pub struct PullDetail {
    generation: u64,
    pull: Pull,
    events: Vec<PullEvent>,
    /// The comparison that shows the pull request's changes.
    base: String,
    head: String,
    source_oid: Option<String>,
    target_oid: Option<String>,
    /// `None` unless open, and for a copy from GitHub that no longer syncs.
    mergeable: Option<Mergeable>,
    conflicts: Vec<String>,
    can_comment: bool,
    can_merge: bool,
    can_close: bool,
}

#[derive(Serialize, TS)]
pub struct Comparison {
    generation: u64,
    base: String,
    head: String,
    merge_base: String,
    /// Newest first.
    commits: Vec<Commit>,
    more: bool,
    diff: String,
    truncated: bool,
}

/// A client's request UUID.
fn request(r: &str) -> Result<String, OpError> {
    uuid::Uuid::parse_str(r)
        .map(|u| u.to_string())
        .map_err(|_| OpError::bad("request must be a UUID"))
}

/// GitHub, for a repository whose pull requests are GitHub's and change
/// there (docs/decisions/0028-two-way-github-mirrors.md).
fn mirror<'a>(s: &'a AppInner, repo: &RepoSettings) -> Option<&'a Github> {
    repo.mirror.as_ref().and(s.github.as_deref())
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct PullsOp {
    repo: String,
    /// Closed and merged ones instead of open ones.
    closed: Option<bool>,
    skip: Option<u32>,
    limit: Option<u32>,
}

impl Op for PullsOp {
    type Answer = PullList;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<PullList, OpError> {
        let repo = readable(s, who, &self.repo).await?;
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_PULLS);
        let (open, skip) = (!self.closed.unwrap_or(false), self.skip.unwrap_or(0));
        let (mut pulls, counts) = s.pulls.db.list(repo.id, open, skip, limit + 1).await?;
        let more = pulls.len() > limit as usize;
        pulls.truncate(limit as usize);
        Ok(PullList {
            pulls,
            counts,
            more,
        })
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PullOp {
    repo: String,
    number: u64,
}

impl Op for PullOp {
    type Answer = PullDetail;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<PullDetail, OpError> {
        let repo = readable(s, who, &self.repo).await?;
        pull(s, who, &repo, self.number).await
    }
}

/// The commits and changes of `head` since its merge base with `base`.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CompareOp {
    repo: String,
    base: String,
    head: String,
}

impl Op for CompareOp {
    type Answer = Comparison;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Comparison, OpError> {
        let (_, g) = open(s, who, &self.repo).await?;
        compare(&g, &self.base, &self.head).await
    }
}

// The operations below change something and need a session.

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct PullCreateOp {
    repo: String,
    source: String,
    target: String,
    title: String,
    body: Option<String>,
    /// A UUID: a retry opens no second pull request.
    request: Option<String>,
}

impl Op for PullCreateOp {
    type Answer = PullChanged;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<PullChanged, OpError> {
        let repo = readable(s, who, &self.repo).await?;
        let Self {
            source,
            target,
            title,
            ..
        } = &self;
        let body = self.body.as_deref().unwrap_or_default();
        let pull = match mirror(s, &repo) {
            Some(github) => {
                github
                    .pull_create(&repo, who, source, target, title, body)
                    .await?
            }
            None => {
                let r = self.request.as_deref().map(request).transpose()?;
                s.pulls
                    .create(&repo, who, source, target, title, body, r)
                    .await?
            }
        };
        Ok(PullChanged { pull })
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct PullCommentOp {
    repo: String,
    number: u64,
    body: String,
    request: Option<String>,
}

impl Op for PullCommentOp {
    type Answer = PullChanged;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<PullChanged, OpError> {
        let repo = readable(s, who, &self.repo).await?;
        let pull = match mirror(s, &repo) {
            Some(github) => {
                github
                    .pull_comment(&repo, who, self.number, &self.body)
                    .await?
            }
            None => {
                let r = self.request.as_deref().map(request).transpose()?;
                s.pulls
                    .comment(&repo, who, self.number, &self.body, r.as_deref())
                    .await?
            }
        };
        Ok(PullChanged { pull })
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PullCloseOp {
    repo: String,
    number: u64,
}

impl Op for PullCloseOp {
    type Answer = PullChanged;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<PullChanged, OpError> {
        set_open(s, who, &self.repo, self.number, false).await
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PullReopenOp {
    repo: String,
    number: u64,
}

impl Op for PullReopenOp {
    type Answer = PullChanged;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<PullChanged, OpError> {
        set_open(s, who, &self.repo, self.number, true).await
    }
}

async fn set_open(
    s: &AppInner,
    who: &Identity,
    name: &str,
    number: u64,
    open: bool,
) -> Result<PullChanged, OpError> {
    let repo = readable(s, who, name).await?;
    let pull = match mirror(s, &repo) {
        Some(github) => github.pull_set_open(&repo, who, number, open).await?,
        None => s.pulls.set_open(&repo, who, number, open).await?,
    };
    Ok(PullChanged { pull })
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct PullMergeOp {
    repo: String,
    number: u64,
    /// The source commit the page showed.
    head: String,
    squash: Option<bool>,
    request: String,
}

impl Op for PullMergeOp {
    type Answer = PullChanged;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<PullChanged, OpError> {
        let repo = readable(s, who, &self.repo).await?;
        let (number, head) = (self.number, &self.head);
        let squash = self.squash.unwrap_or(false);
        let pull = match mirror(s, &repo) {
            Some(github) => github.pull_merge(&repo, who, number, head, squash).await?,
            None => {
                let r = request(&self.request)?;
                s.pulls.merge(&repo, who, number, head, squash, &r).await?
            }
        };
        Ok(PullChanged { pull })
    }
}

/// A pull request with its timeline and the state of its branches. `base`
/// and `head` name the comparison that shows its changes.
async fn pull(
    s: &AppInner,
    who: &Identity,
    repo: &RepoSettings,
    number: u64,
) -> Result<PullDetail, OpError> {
    let p = s
        .pulls
        .db
        .get(repo.id, number)
        .await?
        .ok_or_else(|| OpError::not_found("pull request"))?;
    // GitHub's pull requests follow GitHub, not the branches here.
    let github = p.github_url.is_some();
    // While the mirror syncs, changes here go to GitHub; after **Stop
    // syncing**, the copy stays as it is.
    let live = github && repo.mirror.is_some() && s.github.is_some();
    if p.state != PullState::Merged && !github {
        s.pulls.clone().request_sync(repo.id);
    }
    let events = s.pulls.db.events(repo.id, number).await?;
    let g = Git::open(s, repo).await?;
    let view = &g.view;
    let source = g.commit_oid(&format!("refs/heads/{}", p.source)).await;
    let target = g.commit_oid(&format!("refs/heads/{}", p.target)).await;
    let kept = g.commit_oid(&format!("refs/pull/{number}/head")).await;
    let mut mergeable = None;
    let mut conflicts: Vec<String> = vec![];
    let (head, base) = match p.state {
        // The head is GitHub's ref, which forks have too; GitHub merges
        // it.
        PullState::Open if github => {
            let head = kept.clone().unwrap_or_else(|| p.head_oid.clone());
            let base = match &target {
                Some(t) => g.merge_base(t, &head).await?,
                None => None,
            }
            .unwrap_or_else(|| p.merge_base.clone());
            if live {
                mergeable = Some(match &target {
                    None => Mergeable::TargetMissing,
                    Some(_) if base == head => Mergeable::UpToDate,
                    // The sync has not brought the head yet.
                    Some(_) if kept.is_none() => Mergeable::Clean,
                    Some(t) => {
                        conflicts = merge_check(s, view, t, &head).await?;
                        if conflicts.is_empty() {
                            Mergeable::Clean
                        } else {
                            Mergeable::Conflict
                        }
                    }
                });
            }
            (head, base)
        }
        PullState::Open => {
            let head = source
                .clone()
                .or_else(|| kept.clone())
                .unwrap_or_else(|| p.head_oid.clone());
            let base = match &target {
                Some(t) => g.merge_base(t, &head).await?,
                None => None,
            }
            .unwrap_or_else(|| p.merge_base.clone());
            mergeable = Some(match (&source, &target) {
                (None, _) => Mergeable::SourceMissing,
                (_, None) => Mergeable::TargetMissing,
                (Some(src), Some(t)) => {
                    if base == *src {
                        Mergeable::UpToDate
                    } else {
                        conflicts = merge_check(s, view, t, src).await?;
                        if conflicts.is_empty() {
                            Mergeable::Clean
                        } else {
                            Mergeable::Conflict
                        }
                    }
                }
            });
            // The worker missed a push, or has not finished it yet.
            if source.is_some()
                && (source.as_deref() != Some(p.head_oid.as_str())
                    || target.as_deref() != Some(p.target_oid.as_str())
                    || kept != source)
            {
                s.pulls.clone().request_sync(repo.id);
            }
            (head, base)
        }
        PullState::Closed if github => (p.head_oid.clone(), p.merge_base.clone()),
        PullState::Closed => (
            kept.unwrap_or_else(|| p.head_oid.clone()),
            p.merge_base.clone(),
        ),
        PullState::Merged => (p.head_oid.clone(), p.merge_base.clone()),
    };
    let level = s.directory.access(who, repo);
    let user = match who {
        Identity::Anonymous => None,
        Identity::User { name, .. } => Some(name.as_str()),
    };
    // On GitHub, the author is a GitHub account.
    let author = match (user, &s.github) {
        (Some(u), Some(g)) if live && level < Access::Write => {
            g.db.account(u)
                .await
                .ok()
                .flatten()
                .is_some_and(|a| a.login == p.author)
        }
        (Some(u), _) => !github && u == p.author,
        _ => false,
    };
    let open_here = !github || live;
    Ok(PullDetail {
        generation: view.generation,
        events,
        head,
        base,
        source_oid: source,
        target_oid: target,
        mergeable,
        conflicts,
        can_comment: user.is_some() && open_here,
        can_merge: user.is_some() && level >= Access::Write && open_here,
        can_close: user.is_some() && open_here && (author || level >= Access::Write),
        pull: p,
    })
}

/// The conflicted paths of a merge of `theirs` into `ours`. The merge
/// writes its objects into a temporary directory, never into the cache.
async fn merge_check(
    s: &AppInner,
    view: &crate::repository::ReadView,
    ours: &str,
    theirs: &str,
) -> Result<Vec<String>, OpError> {
    let format = view.format();
    let parse = |o: &str| Oid::parse(format, o).map_err(|e| OpError::bad(e.to_string()));
    let (ours, theirs) = (parse(ours)?, parse(theirs)?);
    let tmp = s
        .registry
        .svc
        .tmp_dir()
        .join(format!("merge-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(tmp.join("pack")).await?;
    let result = s
        .registry
        .svc
        .git
        .merge_tree(&view.dir, Some(&tmp), format, &ours, &theirs)
        .await;
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    Ok(match result? {
        MergeTree::Clean(_) => vec![],
        MergeTree::Conflicts(files) => files,
    })
}

/// The commits of `head` since its merge base with `base`, newest first,
/// and their changes.
async fn compare(g: &Git<'_>, base: &str, head: &str) -> Result<Comparison, OpError> {
    let (base, head) = (g.resolve(base).await?, g.resolve(head).await?);
    let Some(merge_base) = g.merge_base(&base, &head).await? else {
        return Err(OpError::new("unrelated", "the revisions share no history"));
    };
    let range = format!("{merge_base}..{head}");
    let count = format!("--max-count={}", MAX_COMMITS + 1);
    let out = g
        .run(&["log", COMMIT_FORMAT, &count, &range, "--"], None)
        .await?;
    let mut commits = parse_commits(&out);
    let more = commits.len() > MAX_COMMITS;
    commits.truncate(MAX_COMMITS);
    let (diff, truncated) = g
        .run_limited(
            &[
                "diff",
                "--patch",
                "--find-renames",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                &merge_base,
                &head,
            ],
            MAX_DIFF_BYTES,
        )
        .await?;
    Ok(Comparison {
        generation: g.view.generation,
        base,
        head,
        merge_base,
        commits: g.link(commits).await,
        more,
        diff: String::from_utf8_lossy(&diff).into_owned(),
        truncated,
    })
}
