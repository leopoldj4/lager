//! Pull request queries and changes of the web interface
//! (docs/decisions/0019-pull-requests.md).
//!
//! The database answers the pull requests and their timelines; a read view
//! answers their commits, so a page sees every push accepted before the
//! query. Changes go to [`PullService`](crate::pulls::PullService) and need
//! a session.

use serde_json::{Value, json};

use super::read::{COMMIT_FORMAT, Git, MAX_DIFF_BYTES, Query, ReadError, parse_commits};
use crate::git::MergeTree;
use crate::http::AppInner;
use crate::pulls::PullOpError;
use worktree_core::identity::Identity;
use worktree_core::model::Oid;
use worktree_core::pulls::{PullError, PullState};
use worktree_core::repo::{Access, RepoSettings};

const MAX_PULLS: u32 = 100;
/// Commits shown for one comparison.
const MAX_COMMITS: usize = 250;

impl From<PullOpError> for ReadError {
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

impl From<PullError> for ReadError {
    fn from(e: PullError) -> Self {
        PullOpError::from(e).into()
    }
}

/// A client's request UUID.
fn request(r: &str) -> Result<String, ReadError> {
    uuid::Uuid::parse_str(r)
        .map(|u| u.to_string())
        .map_err(|_| ReadError::bad("request must be a UUID"))
}

pub async fn run(
    s: &AppInner,
    who: &Identity,
    repo: &RepoSettings,
    q: Query,
) -> Result<Value, ReadError> {
    let pulls = &s.pulls;
    let changed = |p| Ok(json!({ "pull": p }));
    match q {
        Query::Pulls {
            closed,
            skip,
            limit,
            ..
        } => {
            let limit = limit.clamp(1, MAX_PULLS);
            let (mut list, counts) = pulls.db.list(repo.id, !closed, skip, limit + 1).await?;
            let more = list.len() > limit as usize;
            list.truncate(limit as usize);
            Ok(json!({ "pulls": list, "counts": counts, "more": more }))
        }
        Query::Pull { number, .. } => pull(s, who, repo, number).await,
        Query::Compare { base, head, .. } => compare(s, repo, &base, &head).await,
        Query::PullCreate {
            source,
            target,
            title,
            body,
            request: r,
            ..
        } => {
            let r = r.as_deref().map(request).transpose()?;
            changed(
                pulls
                    .create(repo, who, &source, &target, &title, &body, r)
                    .await?,
            )
        }
        Query::PullComment {
            number,
            body,
            request: r,
            ..
        } => {
            let r = r.as_deref().map(request).transpose()?;
            changed(
                pulls
                    .comment(repo, who, number, &body, r.as_deref())
                    .await?,
            )
        }
        Query::PullClose { number, .. } => changed(pulls.set_open(repo, who, number, false).await?),
        Query::PullReopen { number, .. } => changed(pulls.set_open(repo, who, number, true).await?),
        Query::PullMerge {
            number,
            head,
            squash,
            request: r,
            ..
        } => {
            let r = request(&r)?;
            changed(pulls.merge(repo, who, number, &head, squash, &r).await?)
        }
        _ => Err(ReadError::bad("not a pull request query")),
    }
}

/// A pull request with its timeline and the state of its branches. `base`
/// and `head` name the comparison that shows its changes.
async fn pull(
    s: &AppInner,
    who: &Identity,
    repo: &RepoSettings,
    number: u64,
) -> Result<Value, ReadError> {
    let p = s
        .pulls
        .db
        .get(repo.id, number)
        .await?
        .ok_or_else(|| ReadError::not_found("pull request"))?;
    if p.state != PullState::Merged {
        s.pulls.clone().request_sync(repo.id);
    }
    let events = s.pulls.db.events(repo.id, number).await?;
    let view = s.registry.controller(repo.id).prepare_read().await?;
    let _permits = crate::http::process_permit(s)
        .await
        .map_err(|_| ReadError::new("busy", "server busy; retry later"))?;
    let g = Git { s, view: &view };
    let source = g.commit_oid(&format!("refs/heads/{}", p.source)).await;
    let target = g.commit_oid(&format!("refs/heads/{}", p.target)).await;
    let kept = g.commit_oid(&format!("refs/pull/{number}/head")).await;
    let mut mergeable = Value::Null;
    let mut conflicts: Vec<String> = vec![];
    let (head, base) = match p.state {
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
            mergeable = json!(match (&source, &target) {
                (None, _) => "source_missing",
                (_, None) => "target_missing",
                (Some(src), Some(t)) => {
                    if base == *src {
                        "up_to_date"
                    } else {
                        conflicts = merge_check(s, &view, t, src).await?;
                        if conflicts.is_empty() {
                            "clean"
                        } else {
                            "conflict"
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
    Ok(json!({
        "pull": p,
        "events": events,
        "head": head,
        "base": base,
        "source_oid": source,
        "target_oid": target,
        "mergeable": mergeable,
        "conflicts": conflicts,
        "can_comment": user.is_some(),
        "can_merge": user.is_some() && level >= Access::Write,
        "can_close": user.is_some_and(|u| u == p.author || level >= Access::Write),
        "generation": view.generation,
    }))
}

/// The conflicted paths of a merge of `theirs` into `ours`. The merge
/// writes its objects into a temporary directory, never into the cache.
async fn merge_check(
    s: &AppInner,
    view: &crate::repository::ReadView,
    ours: &str,
    theirs: &str,
) -> Result<Vec<String>, ReadError> {
    let format = view.format();
    let parse = |o: &str| Oid::parse(format, o).map_err(|e| ReadError::bad(e.to_string()));
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
async fn compare(
    s: &AppInner,
    repo: &RepoSettings,
    base: &str,
    head: &str,
) -> Result<Value, ReadError> {
    let view = s.registry.controller(repo.id).prepare_read().await?;
    let _permits = crate::http::process_permit(s)
        .await
        .map_err(|_| ReadError::new("busy", "server busy; retry later"))?;
    let g = Git { s, view: &view };
    let (base, head) = (g.resolve(base).await?, g.resolve(head).await?);
    let Some(merge_base) = g.merge_base(&base, &head).await? else {
        return Err(ReadError::new(
            "unrelated",
            "the revisions share no history",
        ));
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
    Ok(json!({
        "base": base,
        "head": head,
        "merge_base": merge_base,
        "commits": commits,
        "more": more,
        "diff": String::from_utf8_lossy(&diff),
        "truncated": truncated,
        "generation": view.generation,
    }))
}
