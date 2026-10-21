//! Pull requests: their refs, merges, and changes
//! (docs/decisions/0019-pull-requests.md).
//!
//! The database holds the pull requests; storage holds their refs and
//! merges. The server keeps `refs/pull/<n>/head` at the source branch of
//! each open pull request, so its commits stay reachable after the branch
//! moves or goes away. A push never waits for the database: after the
//! server accepts one, [`PullService::run`] records the new commits and
//! moves the refs with one more log entry. A lost event only delays this;
//! the page of a pull request shows the branches themselves and asks for
//! the same work.
//!
//! A merge is a push by the server through the push procedure: the same
//! permission checks, ref rules, packs, CAS, and unknown-result handling.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{Mutex, broadcast, watch};

use crate::bus::Event;
use crate::git::hook::{HookCommand, HookResponse};
use crate::git::{GitError, Ident, MergeTree, NewCommit};
use crate::http::access::Directory;
use crate::repository::{
    AccessCheck, CacheInstance, Origin, PendingPush, PushAccess, Registry, RepoError,
};
use lager_core::identity::Identity;
use lager_core::model::{
    Oid, Operation, RefName, RefUpdate, RepoId, RequestId, branch_ref, pull_receipt_ref, pull_ref,
};
use lager_core::pulls::{Commits, NewPull, Pull, PullError, PullState, Pulls};
use lager_core::repo::{Access, RepoSettings};

/// The user of log entries that only move refs of pull requests. No user
/// name contains `:`.
const SERVER_USER: &str = ":server";
/// Merge attempts while other pushes move the target branch.
const MERGE_ATTEMPTS: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum PullOpError {
    #[error("pull request not found")]
    NotFound,
    #[error("sign in first")]
    SignedOut,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Invalid(String),
    /// The pull request or a branch is not in the state the change needs.
    #[error("{0}")]
    State(String),
    /// The source branch moved since the page showed it.
    #[error("the pull request has new commits; review them before merging")]
    Stale,
    #[error("the branches conflict")]
    Conflict(Vec<String>),
    /// The push procedure refused the change.
    #[error("{0}")]
    Rejected(String),
    #[error("{0}")]
    Busy(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<PullError> for PullOpError {
    fn from(e: PullError) -> Self {
        match e {
            PullError::NotFound => Self::NotFound,
            PullError::Exists(_) | PullError::State(_) => Self::State(e.to_string()),
            PullError::Invalid(m) => Self::Invalid(m),
            PullError::Unavailable(m) => Self::Unavailable(m),
        }
    }
}

impl From<RepoError> for PullOpError {
    fn from(e: RepoError) -> Self {
        match e {
            RepoError::NotFound | RepoError::Deleted => Self::NotFound,
            RepoError::Unavailable(m) => Self::Unavailable(m),
            RepoError::NoSpace(m) => Self::Busy(m),
            RepoError::Internal(m) => Self::Internal(m),
        }
    }
}

impl From<GitError> for PullOpError {
    fn from(e: GitError) -> Self {
        Self::Internal(e.to_string())
    }
}

/// Stored as the message of a commit at refs/server/pull/<n>/receipt.
/// Its parents retain both the actual merge and the reviewed source, even
/// after snapshots, branch deletion, or a database failure.
#[derive(serde::Serialize, serde::Deserialize)]
struct MergeReceipt {
    head: String,
    commit: String,
    base: String,
    by: String,
    request: String,
}

pub struct PullService {
    pub db: Pulls,
    registry: Arc<Registry>,
    directory: Arc<Directory>,
    /// One ref change for pull requests per repository at a time on this
    /// server. Other servers can still change refs; CAS orders them.
    locks: StdMutex<HashMap<RepoId, Arc<Mutex<()>>>>,
    /// Repositories with a requested sync that has not started.
    queued: StdMutex<HashSet<RepoId>>,
}

impl PullService {
    pub fn new(db: Pulls, registry: Arc<Registry>, directory: Arc<Directory>) -> Self {
        Self {
            db,
            registry,
            directory,
            locks: StdMutex::new(HashMap::new()),
            queued: StdMutex::new(HashSet::new()),
        }
    }

    fn lock(&self, repo: RepoId) -> Arc<Mutex<()>> {
        self.locks.lock().unwrap().entry(repo).or_default().clone()
    }

    fn publish(&self, repo: RepoId, p: &Pull) {
        self.registry
            .svc
            .bus
            .publish_pull(repo, p.number, p.version);
    }

    /// Syncs the pull requests of a repository after its pushes on this
    /// server, until the stop signal. Other servers' pushes are theirs.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut events = self.registry.svc.bus.subscribe();
        loop {
            let event = tokio::select! {
                e = events.recv() => e,
                _ = crate::server::stopped(&mut stop) => break,
            };
            match event {
                Ok(Event::Changed(c)) if !c.remote => self.clone().request_sync(c.repo),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Pages of pull requests ask for the missed work.
                    tracing::warn!(missed = n, "pull request refs skipped change events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    /// Starts a sync in the background, unless one waits to start already:
    /// that one sees the newer state too.
    pub fn request_sync(self: Arc<Self>, repo: RepoId) {
        if !self.queued.lock().unwrap().insert(repo) {
            return;
        }
        tokio::spawn(async move {
            let lock = self.lock(repo);
            let _g = lock.lock().await;
            self.queued.lock().unwrap().remove(&repo);
            if let Err(e) = self.sync_locked(repo).await {
                match e {
                    PullOpError::Unavailable(m) => {
                        tracing::debug!(%repo, error = %m, "pull requests not synced")
                    }
                    e => tracing::warn!(%repo, error = %e, "pull requests not synced"),
                }
            }
        });
    }

    /// Records the commits of each open pull request and moves its ref to
    /// the source branch. Marks pull requests merged whose source a push
    /// brought into the target. Holds the repository's lock.
    async fn sync_locked(&self, repo: RepoId) -> Result<(), PullOpError> {
        let open = self.db.unmerged(repo).await?;
        if open.is_empty() {
            return Ok(());
        }
        let git = &self.registry.svc.git;
        let (cache, view, refs) = self.registry.controller(repo).prepare_change().await?;
        let format = cache.format;
        let mut updates = Vec::new();
        let mut ref_guards = Vec::new();
        for p in &open {
            if let Some(oid) = refs.refs.get(&pull_receipt_ref(p.number)) {
                self.recover_merge(repo, p.number, &view.dir, oid).await?;
                continue;
            }
            if p.state != PullState::Open {
                continue;
            }
            let (Ok(source), Ok(target)) = (branch_ref(&p.source), branch_ref(&p.target)) else {
                continue;
            };
            // Without its source branch, a pull request keeps its ref.
            let Some(tip) = refs.refs.get(&source) else {
                continue;
            };
            let pr = pull_ref(p.number);
            if refs.refs.get(&pr) != Some(tip) {
                ref_guards.push((pull_receipt_ref(p.number), None));
                ref_guards.push((source.clone(), Some(tip.clone())));
                updates.push(RefUpdate {
                    name: pr.clone(),
                    old: refs.refs.get(&pr).cloned(),
                    new: Some(tip.clone()),
                });
            }
            let Some(target_tip) = refs.refs.get(&target) else {
                continue;
            };
            if tip.as_str() == p.head_oid && target_tip.as_str() == p.target_oid {
                continue;
            }
            if git.is_ancestor(&view.dir, tip, target_tip).await? {
                // Merged by a push. The diff keeps the merge base of the
                // commits recorded before it.
                if let Some(m) = self
                    .db
                    .mark_merged(
                        repo,
                        p.number,
                        tip.as_str(),
                        target_tip.as_str(),
                        &p.merge_base,
                        None,
                        None,
                    )
                    .await?
                {
                    tracing::info!(%repo, pull = p.number, "pull request merged by a push");
                    self.publish(repo, &m);
                }
                continue;
            }
            let base = git
                .merge_base(&view.dir, format, target_tip, tip)
                .await?
                .map_or_else(|| p.merge_base.clone(), |o| o.to_string());
            let now = Commits {
                head: tip.to_string(),
                target: target_tip.to_string(),
                merge_base: base,
            };
            if let Some(changed) = self
                .db
                .record_commits(repo, p.number, &p.commits(), &now)
                .await?
            {
                self.publish(repo, &changed);
            }
        }
        drop(view);
        if updates.is_empty() {
            return Ok(());
        }
        let access: AccessCheck = Arc::new(move || {
            Box::pin(async move {
                Ok(PushAccess {
                    repo,
                    level: Access::Force,
                    ref_rules: vec![],
                })
            })
        });
        let r = self
            .change_refs(
                repo,
                cache,
                &updates,
                Operation::PullRefs,
                SERVER_USER,
                access,
                ref_guards,
            )
            .await;
        if let Some(why) = failure(&r) {
            // Another server moved them first, or the next push will.
            tracing::debug!(%repo, reason = %why, "pull request refs not moved");
        }
        Ok(())
    }

    /// Runs the push procedure for a change by the server.
    #[allow(clippy::too_many_arguments)]
    async fn change_refs(
        &self,
        repo: RepoId,
        cache: Arc<CacheInstance>,
        updates: &[RefUpdate],
        operation: Operation,
        user: &str,
        access: AccessCheck,
        ref_guards: Vec<(RefName, Option<Oid>)>,
    ) -> HookResponse {
        let format = cache.format;
        let op = PendingPush {
            repo,
            user: user.to_string(),
            request: RequestId::new(),
            access,
            cache,
            default_branch_changed: None,
            origin: Origin::Server(operation),
            ref_guards,
        };
        let commands = updates
            .iter()
            .map(|u| HookCommand {
                name: u.name.to_string(),
                old: Oid::to_git(u.old.as_ref(), format),
                new: Oid::to_git(u.new.as_ref(), format),
            })
            .collect();
        self.registry.controller(repo).push(&op, commands).await
    }

    async fn recover_merge(
        &self,
        repo: RepoId,
        number: u64,
        dir: &std::path::Path,
        oid: &Oid,
    ) -> Result<Pull, PullOpError> {
        let mut cmd = self.registry.svc.git.in_repo(dir);
        cmd.args(["show", "-s", "--format=%B", oid.as_str()]);
        let bytes = crate::git::run(&mut cmd, None).await?;
        let r: MergeReceipt = serde_json::from_slice(&bytes)
            .map_err(|e| PullOpError::Internal(format!("invalid merge receipt: {e}")))?;
        if let Some(p) = self
            .db
            .mark_merged(
                repo,
                number,
                &r.head,
                &r.commit,
                &r.base,
                Some(&r.by),
                Some(&r.request),
            )
            .await?
        {
            self.publish(repo, &p);
            return Ok(p);
        }
        self.db
            .get(repo, number)
            .await?
            .ok_or(PullOpError::NotFound)
    }

    /// Opens a pull request to merge `source` into `target`.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        self: &Arc<Self>,
        repo: &RepoSettings,
        who: &Identity,
        source: &str,
        target: &str,
        title: &str,
        body: &str,
        request: Option<String>,
    ) -> Result<Pull, PullOpError> {
        let user = signed_in(who)?;
        if self.directory.access(who, repo) < Access::Read {
            return Err(PullOpError::NotFound);
        }
        if let Some(m) = &repo.mirror {
            return Err(PullOpError::Forbidden(format!(
                "this repository is a read-only mirror of github.com/{m}"
            )));
        }
        lager_core::pulls::validate_text(Some(title), body)?;
        if source == target {
            return Err(PullOpError::Invalid("choose two different branches".into()));
        }
        let (source_ref, target_ref) = (branch(source)?, branch(target)?);
        let git = &self.registry.svc.git;
        let (cache, view, refs) = self.registry.controller(repo.id).prepare_change().await?;
        let tip = refs
            .refs
            .get(&source_ref)
            .ok_or_else(|| PullOpError::Invalid(format!("branch {source} not found")))?;
        let target_tip = refs
            .refs
            .get(&target_ref)
            .ok_or_else(|| PullOpError::Invalid(format!("branch {target} not found")))?;
        if git.is_ancestor(&view.dir, tip, target_tip).await? {
            return Err(PullOpError::Invalid(format!(
                "{target} already contains every commit of {source}"
            )));
        }
        let base = git
            .merge_base(&view.dir, cache.format, target_tip, tip)
            .await?
            .ok_or_else(|| {
                PullOpError::Invalid(format!("{source} and {target} share no history"))
            })?;
        drop((cache, view));
        let p = self
            .db
            .create(
                repo.id,
                NewPull {
                    title: title.to_string(),
                    body: body.to_string(),
                    author: user.to_string(),
                    source: source.to_string(),
                    target: target.to_string(),
                    head_oid: tip.to_string(),
                    target_oid: target_tip.to_string(),
                    merge_base: base.to_string(),
                    request,
                },
            )
            .await?;
        tracing::info!(repo = %repo.name, pull = p.number, user, "pull request opened");
        self.publish(repo.id, &p);
        self.clone().request_sync(repo.id);
        Ok(p)
    }

    pub async fn comment(
        &self,
        repo: &RepoSettings,
        who: &Identity,
        number: u64,
        body: &str,
        request: Option<&str>,
    ) -> Result<Pull, PullOpError> {
        let user = signed_in(who)?;
        if self.directory.access(who, repo) < Access::Read {
            return Err(PullOpError::NotFound);
        }
        let p = self
            .db
            .comment(repo.id, number, user, body, request)
            .await?;
        self.publish(repo.id, &p);
        Ok(p)
    }

    /// Closes a pull request, or opens it again: its author or a user with
    /// write access.
    pub async fn set_open(
        self: &Arc<Self>,
        repo: &RepoSettings,
        who: &Identity,
        number: u64,
        open: bool,
    ) -> Result<Pull, PullOpError> {
        let user = signed_in(who)?;
        let level = self.directory.access(who, repo);
        if level < Access::Read {
            return Err(PullOpError::NotFound);
        }
        let p = self
            .db
            .get(repo.id, number)
            .await?
            .ok_or(PullOpError::NotFound)?;
        if p.author != user && level < Access::Write {
            return Err(PullOpError::Forbidden(
                "only the author and users with write access can do this".into(),
            ));
        }
        let p = self.db.set_open(repo.id, number, open, user).await?;
        self.publish(repo.id, &p);
        if open {
            self.clone().request_sync(repo.id);
        }
        Ok(p)
    }

    /// Merges an open pull request whose source branch is still at
    /// `expected_head`. `squash`: one commit on the target instead of a
    /// merge commit. A retry with the same request answers the merge.
    pub async fn merge(
        &self,
        repo: &RepoSettings,
        who: &Identity,
        number: u64,
        expected_head: &str,
        squash: bool,
        request: &str,
    ) -> Result<Pull, PullOpError> {
        let user = signed_in(who)?;
        let level = self.directory.access(who, repo);
        if level < Access::Read {
            return Err(PullOpError::NotFound);
        }
        if level < Access::Write {
            return Err(PullOpError::Forbidden("merging needs write access".into()));
        }
        let git = &self.registry.svc.git;
        let controller = self.registry.controller(repo.id);
        // The sync after this merge's push sees the pull request merged.
        let lock = self.lock(repo.id);
        let _g = lock.lock().await;
        for _ in 0..MERGE_ATTEMPTS {
            let p = self
                .db
                .get(repo.id, number)
                .await?
                .ok_or(PullOpError::NotFound)?;
            let (cache, view, refs) = controller.prepare_change().await?;
            if let Some(oid) = refs.refs.get(&pull_receipt_ref(number)) {
                let merged = self.recover_merge(repo.id, number, &view.dir, oid).await?;
                return if merged.merge_request.as_deref() == Some(request) {
                    Ok(merged)
                } else {
                    Err(PullOpError::State("the pull request is merged".into()))
                };
            }
            match p.state {
                PullState::Merged if p.merge_request.as_deref() == Some(request) => return Ok(p),
                PullState::Merged => {
                    return Err(PullOpError::State("the pull request is merged".into()));
                }
                PullState::Closed => {
                    return Err(PullOpError::State("the pull request is closed".into()));
                }
                PullState::Open => {}
            }
            let (source, target) = (branch(&p.source)?, branch(&p.target)?);
            let format = cache.format;
            let tip =
                refs.refs.get(&source).cloned().ok_or_else(|| {
                    PullOpError::State(format!("branch {} was deleted", p.source))
                })?;
            if tip.as_str() != expected_head {
                return Err(PullOpError::Stale);
            }
            let target_tip =
                refs.refs.get(&target).cloned().ok_or_else(|| {
                    PullOpError::State(format!("branch {} was deleted", p.target))
                })?;
            if git.is_ancestor(&view.dir, &tip, &target_tip).await? {
                // A push merged it already.
                drop((cache, view));
                self.sync_locked(repo.id).await?;
                return self
                    .db
                    .get(repo.id, number)
                    .await?
                    .ok_or(PullOpError::NotFound);
            }
            let base = git
                .merge_base(&view.dir, format, &target_tip, &tip)
                .await?
                .ok_or_else(|| PullOpError::State("the branches share no history".into()))?;
            drop(view);
            // New objects go into the cache, as received ones do.
            let tree = match git
                .merge_tree(&cache.dir, None, format, &target_tip, &tip)
                .await?
            {
                MergeTree::Clean(t) => t,
                MergeTree::Conflicts(files) => return Err(PullOpError::Conflict(files)),
            };
            let me = Ident {
                name: user.to_string(),
                email: String::new(),
            };
            // A squash keeps the pull request's author.
            let author = Ident {
                name: p.author.clone(),
                email: String::new(),
            };
            let (parents, message, author) = if squash {
                (vec![&target_tip], squash_message(&p), &author)
            } else {
                (vec![&target_tip, &tip], merge_message(&p), &me)
            };
            let new = NewCommit {
                tree: &tree,
                parents: &parents,
                message: &message,
                author,
                committer: &me,
            };
            let commit = git.commit_tree(&cache.dir, format, &new).await?;
            let receipt_message = serde_json::to_string(&MergeReceipt {
                head: tip.to_string(),
                commit: commit.to_string(),
                base: base.to_string(),
                by: user.to_string(),
                request: request.to_string(),
            })
            .map_err(|e| PullOpError::Internal(e.to_string()))?;
            let receipt = git
                .commit_tree(
                    &cache.dir,
                    format,
                    &NewCommit {
                        tree: &tree,
                        parents: &[&commit, &tip],
                        message: &receipt_message,
                        author: &me,
                        committer: &me,
                    },
                )
                .await?;
            let pr = pull_ref(number);
            let mut updates = vec![RefUpdate {
                name: target.clone(),
                old: Some(target_tip.clone()),
                new: Some(commit.clone()),
            }];
            if refs.refs.get(&pr) != Some(&tip) {
                updates.push(RefUpdate {
                    name: pr.clone(),
                    old: refs.refs.get(&pr).cloned(),
                    new: Some(tip.clone()),
                });
            }
            updates.push(RefUpdate {
                name: pull_receipt_ref(number),
                old: None,
                new: Some(receipt),
            });
            let access =
                crate::http::push_access(self.directory.clone(), who.clone(), repo.name.clone());
            let r = self
                .change_refs(
                    repo.id,
                    cache,
                    &updates,
                    Operation::Merge { pull: number },
                    user,
                    access,
                    vec![(source.clone(), Some(tip.clone()))],
                )
                .await;
            let Some(why) = failure(&r) else {
                tracing::info!(repo = %repo.name, pull = number, user, squash, "pull request merged");
                return match self
                    .db
                    .mark_merged(
                        repo.id,
                        number,
                        tip.as_str(),
                        commit.as_str(),
                        base.as_str(),
                        Some(user),
                        Some(request),
                    )
                    .await
                {
                    Ok(Some(p)) => {
                        self.publish(repo.id, &p);
                        Ok(p)
                    }
                    // Another server already recorded the receipt.
                    Ok(None) => self
                        .db
                        .get(repo.id, number)
                        .await?
                        .ok_or(PullOpError::NotFound),
                    Err(e) => {
                        // The next sync recovers the durable merge receipt.
                        tracing::warn!(repo = %repo.name, pull = number, error = %e, "merge not recorded");
                        Err(PullOpError::Unavailable(
                            "merged; the pull request shows it shortly".into(),
                        ))
                    }
                };
            };
            // Another push moved a branch: merge again against it.
            let (_, _, now) = controller.prepare_change().await?;
            if now.refs.get(&target) != Some(&target_tip)
                || now.refs.get(&source) != Some(&tip)
                || now.refs.contains_key(&pull_receipt_ref(number))
            {
                continue;
            }
            return Err(PullOpError::Rejected(why));
        }
        Err(PullOpError::Busy(
            "the target branch keeps changing; merge again".into(),
        ))
    }
}

fn signed_in(who: &Identity) -> Result<&str, PullOpError> {
    match who {
        Identity::Anonymous => Err(PullOpError::SignedOut),
        Identity::User { name, .. } => Ok(name),
    }
}

fn branch(short: &str) -> Result<RefName, PullOpError> {
    branch_ref(short).map_err(|_| PullOpError::Invalid(format!("invalid branch name {short:?}")))
}

/// The first reason of a refused change.
fn failure(r: &HookResponse) -> Option<String> {
    if r.results.iter().all(|x| x.ok) {
        return None;
    }
    Some(
        r.results
            .iter()
            .find_map(|x| x.message.clone())
            .unwrap_or_else(|| "rejected".into()),
    )
}

fn merge_message(p: &Pull) -> String {
    format!(
        "Merge pull request #{} from {}\n\n{}\n",
        p.number, p.source, p.title
    )
}

fn squash_message(p: &Pull) -> String {
    let body = p.body.trim();
    if body.is_empty() {
        format!("{} (#{})\n", p.title, p.number)
    } else {
        format!("{} (#{})\n\n{body}\n", p.title, p.number)
    }
}
