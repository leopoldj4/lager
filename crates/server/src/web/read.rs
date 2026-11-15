//! Read queries of the web interface: refs, history, trees, files, and
//! commits (docs/decisions/0014-web-interface.md). Queries of pull requests
//! are in [`super::pulls`].
//!
//! Each query reads a [`ReadView`](crate::repository::ReadView), so it sees
//! every push accepted before it, and answers with the view's generation.
//! The cache also holds objects of rejected pushes: a query names a branch,
//! a tag, or a commit that a ref reaches, never any other object.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gix::bstr::ByteSlice;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::ops::{DEFAULT_LIMIT, Op, OpError, readable};
use crate::http::AppInner;
use crate::repository::{ReadView, RepoError};
use lager_core::identity::Identity;
use lager_core::model::RefName;
use lager_core::repo::RepoSettings;
use ts_rs::TS;

/// Files above this size are shown without contents.
const MAX_BLOB_BYTES: u64 = 1024 * 1024;
/// The raw route refuses larger files.
const MAX_RAW_BYTES: u64 = 20 * 1024 * 1024;
pub(super) const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_TREE_ENTRIES: usize = 5000;
const MAX_LOG: u32 = 100;
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Objects kept decoded per read, for walks over history.
const OBJECT_CACHE_BYTES: usize = 4 * 1024 * 1024;

/// Who wrote or committed a commit.
#[derive(Serialize, TS)]
pub struct Person {
    name: String,
    email: String,
    time: i64,
    /// The user whose email this is, on commit authors.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    user: Option<String>,
}

#[derive(Serialize, TS)]
pub struct Commit {
    oid: String,
    parents: Vec<String>,
    author: Person,
    committer: Person,
    subject: String,
}

/// A commit as Git tells it, its author not looked up among the users.
/// [`Git::link`] makes the [`Commit`]s of an answer from these.
pub(super) struct Unlinked(Commit);

#[derive(Serialize, TS)]
pub struct RefInfo {
    name: String,
    oid: String,
    time: i64,
    subject: String,
}

#[derive(Serialize, TS)]
pub struct Refs {
    generation: u64,
    /// The GitHub repository of a mirror.
    mirror: Option<String>,
    /// Anyone may read it, also without signing in.
    public: bool,
    head: String,
    branches: Vec<RefInfo>,
    tags: Vec<RefInfo>,
}

#[derive(Serialize, TS, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Tree,
    File,
    Symlink,
    Submodule,
}

#[derive(Serialize, TS)]
pub struct TreeEntry {
    name: String,
    kind: EntryKind,
    mode: String,
    oid: String,
    size: Option<u64>,
}

#[derive(Serialize, TS)]
pub struct Tree {
    generation: u64,
    oid: String,
    path: String,
    entries: Vec<TreeEntry>,
    truncated: bool,
    last_commit: Option<Commit>,
}

#[derive(Serialize, TS)]
pub struct Blob {
    generation: u64,
    oid: String,
    path: String,
    blob: String,
    size: u64,
    binary: bool,
    too_large: bool,
    text: Option<String>,
    last_commit: Option<Commit>,
}

#[derive(Serialize, TS)]
pub struct Log {
    generation: u64,
    oid: String,
    commits: Vec<Commit>,
    more: bool,
}

/// A commit with its whole message.
#[derive(Serialize, TS)]
pub struct FullCommit {
    #[serde(flatten)]
    commit: Commit,
    message: String,
}

#[derive(Serialize, TS)]
pub struct CommitDetail {
    generation: u64,
    commit: FullCommit,
    diff: String,
    truncated: bool,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RefsOp {
    repo: String,
}

impl Op for RefsOp {
    type Answer = Refs;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Refs, OpError> {
        let (repo, g) = open(s, who, &self.repo).await?;
        g.refs(&repo).await
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct LogOp {
    repo: String,
    rev: String,
    path: Option<String>,
    skip: Option<u32>,
    limit: Option<u32>,
}

impl Op for LogOp {
    type Answer = Log;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Log, OpError> {
        let (_, g) = open(s, who, &self.repo).await?;
        let oid = g.resolve(&self.rev).await?;
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LOG);
        let (path, skip) = (self.path.unwrap_or_default(), self.skip.unwrap_or(0));
        g.log(&oid, &path, skip, limit).await
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct TreeOp {
    repo: String,
    rev: String,
    path: Option<String>,
}

impl Op for TreeOp {
    type Answer = Tree;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Tree, OpError> {
        let (_, g) = open(s, who, &self.repo).await?;
        let oid = g.resolve(&self.rev).await?;
        g.tree(&oid, self.path.as_deref().unwrap_or_default()).await
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BlobOp {
    repo: String,
    rev: String,
    path: String,
}

impl Op for BlobOp {
    type Answer = Blob;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Blob, OpError> {
        let (_, g) = open(s, who, &self.repo).await?;
        let oid = g.resolve(&self.rev).await?;
        g.blob(&oid, &self.path).await
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CommitOp {
    repo: String,
    rev: String,
}

impl Op for CommitOp {
    type Answer = CommitDetail;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<CommitDetail, OpError> {
        let (_, g) = open(s, who, &self.repo).await?;
        let oid = g.resolve(&self.rev).await?;
        g.commit(&oid).await
    }
}

impl From<gix::Error> for OpError {
    fn from(e: gix::Error) -> Self {
        RepoError::Internal(format!("read: {e}")).into()
    }
}

/// The repository `name` if `who` may read it, opened for Git reads.
pub(super) async fn open<'a>(
    s: &'a AppInner,
    who: &Identity,
    name: &str,
) -> Result<(RepoSettings, Git<'a>), OpError> {
    let repo = readable(s, who, name).await?;
    let g = Git::open(s, &repo).await?;
    Ok((repo, g))
}

/// A file for the raw route.
pub struct RawFile {
    pub path: String,
    /// The blob's object ID.
    pub blob: String,
    /// `None` when the browser already holds this blob.
    pub data: Option<Vec<u8>>,
}

/// A file of `<rev>/<path>` for the raw route. The address is split at the
/// longest branch or tag name, as the pages split theirs. `cached`: the blob
/// the browser holds; its contents are not read again.
pub async fn raw(
    s: &AppInner,
    who: &Identity,
    name: &str,
    rest: &str,
    cached: Option<&str>,
) -> Result<RawFile, OpError> {
    let (_, g) = open(s, who, name).await?;
    let (rev, path) = g.split_rev(rest).await?;
    let oid = g.resolve(rev).await?;
    let path = check_path(path)?;
    if path.is_empty() {
        return Err(OpError::bad("path required"));
    }
    let (blob, size) = g.blob_info(&oid, path).await?;
    let data = if cached == Some(blob.as_str()) {
        None
    } else if size > MAX_RAW_BYTES {
        return Err(OpError::new("too_large", "file too large"));
    } else {
        Some(g.read_blob(&blob).await?)
    };
    Ok(RawFile {
        path: path.to_string(),
        blob,
        data,
    })
}

/// Commit fields for `--format`, separated by NUL; records end with RS.
pub(super) const COMMIT_FORMAT: &str =
    "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%s%x1e";

pub(super) fn parse_commits(out: &[u8]) -> Vec<Unlinked> {
    let person = |name: &str, email: &str, time: &str| Person {
        name: name.to_string(),
        email: email.to_string(),
        time: time.parse().unwrap_or(0),
        user: None,
    };
    String::from_utf8_lossy(out)
        .split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            let f: Vec<&str> = rec.splitn(9, '\0').collect();
            let [oid, parents, an, ae, at, cn, ce, ct, subject] = f[..] else {
                return None;
            };
            Some(Unlinked(Commit {
                oid: oid.to_string(),
                parents: parents.split_whitespace().map(str::to_string).collect(),
                author: person(an, ae, at),
                committer: person(cn, ce, ct),
                subject: subject.to_string(),
            }))
        })
        .collect()
}

/// A path inside a tree: `/`-separated names, no `.` or `..`.
fn check_path(path: &str) -> Result<&str, OpError> {
    let path = path.trim_matches('/');
    if path.is_empty() {
        return Ok(path);
    }
    let ok = path.len() <= 4096
        && path.split('/').all(|c| {
            !c.is_empty() && c != "." && c != ".." && !c.bytes().any(|b| b < 0x20 || b == 0x7f)
        });
    if ok {
        Ok(path)
    } else {
        Err(OpError::bad("invalid path"))
    }
}

/// Git reads of a repository as of one generation.
pub(super) struct Git<'a> {
    s: &'a AppInner,
    pub(super) view: Arc<ReadView>,
    _permits: crate::http::process::Guards,
}

impl<'a> Git<'a> {
    /// A view with every push accepted so far, and a place among the Git
    /// processes of the server.
    pub(super) async fn open(s: &'a AppInner, repo: &RepoSettings) -> Result<Self, OpError> {
        let view = s.registry.controller(repo.id).prepare_read().await?;
        let _permits = crate::http::process_permit(s)
            .await
            .map_err(|_| OpError::new("busy", "server busy; retry later"))?;
        Ok(Self { s, view, _permits })
    }

    /// The commits for an answer: `user` on the authors whose email
    /// belongs to a user, so their names link to the profile. The lookup
    /// sees only the emails of commits that the reader may read already.
    pub(super) async fn link(&self, commits: Vec<Unlinked>) -> Vec<Commit> {
        let mut commits: Vec<Commit> = commits.into_iter().map(|c| c.0).collect();
        let mut emails: Vec<String> = commits
            .iter()
            .map(|c| c.author.email.to_lowercase())
            .collect();
        emails.sort();
        emails.dedup();
        if emails.is_empty() {
            return commits;
        }
        let owners: HashMap<String, String> = match self.s.directory.users().owners(&emails).await {
            Ok(o) => o.into_iter().collect(),
            Err(e) => {
                // The names stay plain text.
                tracing::warn!(error = ?e, "user store unavailable");
                return commits;
            }
        };
        for c in &mut commits {
            c.author.user = owners.get(&c.author.email.to_lowercase()).cloned();
        }
        commits
    }
}

/// The end of a query's time; walks over history check it.
#[derive(Clone, Copy)]
struct Deadline(Instant);

impl Deadline {
    fn check(self) -> Result<(), OpError> {
        if Instant::now() > self.0 {
            return Err(OpError::new("unavailable", "query took too long"));
        }
        Ok(())
    }
}

/// A commit as `parse_commits` answers it.
fn commit_info(c: &gix::Commit<'_>) -> Result<Unlinked, OpError> {
    let person = |p: gix::actor::SignatureRef<'_>| {
        let p = p.trim();
        Person {
            name: p.name.to_str_lossy().into_owned(),
            email: p.email.to_str_lossy().into_owned(),
            time: p.seconds(),
            user: None,
        }
    };
    Ok(Unlinked(Commit {
        oid: c.id.to_string(),
        parents: c.parent_ids().map(|p| p.to_string()).collect(),
        author: person(c.author()?),
        committer: person(c.committer()?),
        subject: subject(c.message_raw_sloppy()),
    }))
}

/// The first paragraph of a message on one line, as Git's `%s`.
fn subject(message: &[u8]) -> String {
    gix::objs::commit::MessageRef::from_bytes(message)
        .summary()
        .to_str_lossy()
        .into_owned()
}

/// The commit that a full object ID or a full ref name names.
fn find_commit(repo: &gix::Repository, spec: &str) -> Option<gix::ObjectId> {
    let id = match gix::ObjectId::from_hex(spec.as_bytes()) {
        Ok(id) => id,
        Err(_) => repo
            .try_find_reference(spec)
            .ok()??
            .into_fully_peeled_id()
            .ok()?
            .detach(),
    };
    Some(repo.try_find_object(id).ok()??.peel_to_commit().ok()?.id)
}

/// Whether any ref reaches the commit `oid`.
fn reached(
    repo: &gix::Repository,
    oid: gix::ObjectId,
    deadline: Deadline,
) -> Result<bool, OpError> {
    let mut tips = vec![];
    for r in repo.references()?.all()? {
        let Ok(id) = r?.into_fully_peeled_id() else {
            continue;
        };
        if id == oid {
            return Ok(true);
        }
        if matches!(repo.find_header(id), Ok(h) if h.kind() == gix::objs::Kind::Commit) {
            tips.push(id.detach());
        }
    }
    if tips.is_empty() {
        return Ok(false);
    }
    for info in repo.rev_walk(tips).all()? {
        deadline.check()?;
        if info?.id == oid {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A full object ID that `resolve` or a tree answered.
fn object_id(oid: &str) -> Result<gix::ObjectId, OpError> {
    gix::ObjectId::from_hex(oid.as_bytes())
        .map_err(|e| RepoError::Internal(format!("object ID {oid}: {e}")).into())
}

/// The entry at `path` in the tree of `commit`; the root has none.
fn entry_at<'r>(
    repo: &'r gix::Repository,
    commit: gix::ObjectId,
    path: &str,
) -> Result<Option<gix::object::tree::Entry<'r>>, OpError> {
    let tree = repo.find_commit(commit)?.tree()?;
    Ok(tree.lookup_entry(path.split('/').map(|c| c.as_bytes()))?)
}

impl Git<'_> {
    fn cmd(&self) -> Command {
        let mut c = self.s.registry.svc.git.service_command();
        c.env("GIT_DIR", &self.view.dir);
        c
    }

    /// Runs `f` with the view off the async runtime, within the query time.
    async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&gix::Repository, Deadline) -> Result<T, OpError> + Send + 'static,
    ) -> Result<T, OpError> {
        let view = self.view.clone();
        let deadline = Deadline(Instant::now() + GIT_TIMEOUT);
        tokio::task::spawn_blocking(move || {
            let mut repo = view.open()?;
            repo.object_cache_size_if_unset(OBJECT_CACHE_BYTES);
            f(&repo, deadline)
        })
        .await
        .map_err(|e| RepoError::Internal(format!("read task: {e}")))?
    }

    pub(super) async fn run(
        &self,
        args: &[&str],
        input: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, OpError> {
        let mut c = self.cmd();
        c.args(args);
        match tokio::time::timeout(GIT_TIMEOUT, crate::git::run(&mut c, input)).await {
            Ok(r) => Ok(r?),
            Err(_) => Err(OpError::new("unavailable", "query took too long")),
        }
    }

    /// Like [`run`](Self::run), but keeps at most `max` bytes of output and
    /// stops Git when there is more.
    pub(super) async fn run_limited(
        &self,
        args: &[&str],
        max: usize,
    ) -> Result<(Vec<u8>, bool), OpError> {
        let mut c = self.cmd();
        c.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = c.spawn()?;
        let mut stdout = child.stdout.take().expect("piped");
        let mut out = Vec::new();
        let read = async {
            (&mut stdout)
                .take(max as u64 + 1)
                .read_to_end(&mut out)
                .await
        };
        match tokio::time::timeout(GIT_TIMEOUT, read).await {
            Ok(r) => {
                r?;
            }
            Err(_) => return Err(OpError::new("unavailable", "query took too long")),
        }
        let truncated = out.len() > max;
        if truncated {
            out.truncate(max);
            let _ = child.kill().await;
        } else {
            drop(stdout);
            let status = child.wait().await?;
            if !status.success() {
                return Err(RepoError::Internal(format!("git {} failed", args[0])).into());
            }
        }
        Ok((out, truncated))
    }

    /// Whether `spec`, a full object ID or a full ref name, names a commit;
    /// answers its object ID.
    pub(super) async fn commit_oid(&self, spec: &str) -> Option<String> {
        let spec = spec.to_string();
        let found = self.read(move |repo, _| Ok(find_commit(repo, &spec))).await;
        found.ok().flatten().map(|id| id.to_string())
    }

    /// The commit for a branch name, a tag name, a full ref name, or the
    /// object ID of a commit that a ref reaches.
    pub(super) async fn resolve(&self, rev: &str) -> Result<String, OpError> {
        let is_oid =
            (rev.len() == 40 || rev.len() == 64) && rev.bytes().all(|b| b.is_ascii_hexdigit());
        if is_oid {
            let rev = rev.to_ascii_lowercase();
            return self
                .read(move |repo, deadline| match find_commit(repo, &rev) {
                    Some(oid) if reached(repo, oid, deadline)? => Ok(oid.to_string()),
                    _ => Err(OpError::not_found("commit")),
                })
                .await;
        }
        let candidates: Vec<String> = if rev.starts_with("refs/") {
            vec![rev.to_string()]
        } else {
            vec![format!("refs/heads/{rev}"), format!("refs/tags/{rev}")]
        };
        for c in &candidates {
            if RefName::parse(c).is_err() {
                return Err(OpError::bad("invalid ref name"));
            }
        }
        self.read(move |repo, _| {
            candidates
                .iter()
                .find_map(|c| find_commit(repo, c))
                .map(|oid| oid.to_string())
                .ok_or_else(|| OpError::not_found("branch or tag"))
        })
        .await
    }

    /// Splits `<rev>/<path>` at the longest branch or tag name, else after
    /// the first segment.
    async fn split_rev<'r>(&self, rest: &'r str) -> Result<(&'r str, &'r str), OpError> {
        let first = rest.split_once('/').unwrap_or((rest, ""));
        if !first.1.contains('/') {
            return Ok(first);
        }
        let names = self
            .read(|repo, _| {
                let refs = repo.references()?;
                let mut names = std::collections::HashSet::new();
                for r in refs.local_branches()?.chain(refs.tags()?) {
                    let r = r?;
                    let name = r.name().as_bstr().to_str_lossy().into_owned();
                    let short = name
                        .strip_prefix("refs/heads/")
                        .or_else(|| name.strip_prefix("refs/tags/"))
                        .map(str::to_string);
                    names.extend(short);
                }
                Ok(names)
            })
            .await?;
        let longest = rest
            .rmatch_indices('/')
            .map(|(i, _)| i)
            .find(|&i| names.contains(&rest[..i]));
        Ok(longest.map_or(first, |i| (&rest[..i], &rest[i + 1..])))
    }

    /// The best common ancestor of two commits; `None` without one.
    pub(super) async fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>, OpError> {
        let out = self
            .cmd()
            .args(["merge-base", a, b])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        let out = match tokio::time::timeout(GIT_TIMEOUT, out).await {
            Ok(r) => r?,
            Err(_) => return Err(OpError::new("unavailable", "query took too long")),
        };
        match out.status.code() {
            Some(0) => Ok(Some(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )),
            Some(1) => Ok(None),
            _ => Err(RepoError::Internal("git merge-base failed".into()).into()),
        }
    }

    async fn refs(&self, settings: &RepoSettings) -> Result<Refs, OpError> {
        let generation = self.view.generation;
        let (mirror, public) = (settings.mirror.clone(), settings.anonymous_read);
        self.read(move |repo, _| {
            let refs = repo.references()?;
            let (mut branches, mut tags) = (vec![], vec![]);
            for r in refs.local_branches()?.chain(refs.tags()?) {
                let r = r?;
                let name = r.name().as_bstr().to_str_lossy().into_owned();
                let Some(target) = r.try_id() else { continue };
                let object = repo.find_object(target)?;
                // Annotated tags: their own date and message, and the commit.
                let (time, subject) = match object.kind {
                    gix::objs::Kind::Commit => {
                        let c = object.clone().into_commit();
                        (c.committer()?.seconds(), subject(c.message_raw_sloppy()))
                    }
                    gix::objs::Kind::Tag => {
                        let t = object.clone().into_tag();
                        let time = t.tagger()?.map_or(0, |p| p.seconds());
                        (time, subject(t.decode()?.message))
                    }
                    _ => (0, String::new()),
                };
                let oid = object.peel_tags_to_end()?.id;
                let entry = |short: &str| RefInfo {
                    name: short.to_string(),
                    oid: oid.to_string(),
                    time,
                    subject,
                };
                if let Some(b) = name.strip_prefix("refs/heads/") {
                    branches.push(entry(b));
                } else if let Some(t) = name.strip_prefix("refs/tags/") {
                    tags.push(entry(t));
                }
            }
            branches.sort_by(|a, b| a.name.cmp(&b.name));
            tags.sort_by(|a, b| a.name.cmp(&b.name));
            let head = repo
                .head_name()?
                .map(|n| n.as_bstr().to_str_lossy().into_owned())
                .unwrap_or_default();
            Ok(Refs {
                generation,
                mirror,
                public,
                head: head
                    .strip_prefix("refs/heads/")
                    .unwrap_or(&head)
                    .to_string(),
                branches,
                tags,
            })
        })
        .await
    }

    /// The history of a commit, newest first. The history of a path stays
    /// with Git, whose simplification of merges the pages show.
    async fn log(&self, oid: &str, path: &str, skip: u32, limit: u32) -> Result<Log, OpError> {
        let path = check_path(path)?;
        let mut commits = if path.is_empty() {
            let start = object_id(oid)?;
            self.read(move |repo, deadline| {
                let walk = repo
                    .rev_walk([start])
                    .sorting(gix::revision::walk::Sorting::ByCommitTime(
                        Default::default(),
                    ))
                    .all()?;
                let mut commits = vec![];
                for info in walk.skip(skip as usize).take(limit as usize + 1) {
                    deadline.check()?;
                    commits.push(commit_info(&info?.object()?)?);
                }
                Ok(commits)
            })
            .await?
        } else {
            let skip = format!("--skip={skip}");
            let count = format!("--max-count={}", limit + 1);
            let args = ["log", COMMIT_FORMAT, &skip, &count, oid, "--", path];
            parse_commits(&self.run(&args, None).await?)
        };
        let more = commits.len() > limit as usize;
        commits.truncate(limit as usize);
        Ok(Log {
            generation: self.view.generation,
            oid: oid.to_string(),
            commits: self.link(commits).await,
            more,
        })
    }

    /// The last commit that changed `path`: the commit itself for the root.
    async fn last_commit(&self, oid: &str, path: &str) -> Result<Option<Commit>, OpError> {
        let last = if path.is_empty() {
            let id = object_id(oid)?;
            self.read(move |repo, _| Ok(vec![commit_info(&repo.find_commit(id)?)?]))
                .await?
        } else {
            let args = ["log", COMMIT_FORMAT, "--max-count=1", oid, "--", path];
            parse_commits(&self.run(&args, None).await?)
        };
        Ok(self.link(last).await.pop())
    }

    async fn tree(&self, oid: &str, path: &str) -> Result<Tree, OpError> {
        let path = check_path(path)?;
        let commit = object_id(oid)?;
        let owned = path.to_string();
        let entries = self.read(move |repo, _| {
            let tree = if owned.is_empty() {
                repo.find_commit(commit)?.tree()?
            } else {
                match entry_at(repo, commit, &owned)? {
                    Some(e) if e.mode().is_tree() => repo.find_tree(e.object_id())?,
                    Some(_) => return Err(OpError::new("not_a_tree", "path is a file")),
                    None => return Err(OpError::not_found("path")),
                }
            };
            let mut entries = vec![];
            let mut truncated = false;
            for e in tree.iter() {
                if entries.len() == MAX_TREE_ENTRIES {
                    truncated = true;
                    break;
                }
                let e = e?;
                let mode = e.mode();
                let kind = if mode.is_tree() {
                    EntryKind::Tree
                } else if mode.is_commit() {
                    EntryKind::Submodule
                } else if mode.is_link() {
                    EntryKind::Symlink
                } else {
                    EntryKind::File
                };
                let size = if mode.is_blob_or_symlink() {
                    repo.find_header(e.object_id()).ok().map(|h| h.size())
                } else {
                    None
                };
                entries.push(TreeEntry {
                    name: e.filename().to_str_lossy().into_owned(),
                    kind,
                    mode: format!("{:0>6}", mode.kind().as_octal_str().to_str_lossy()),
                    oid: e.object_id().to_string(),
                    size,
                });
            }
            entries.sort_by(|a, b| {
                let file = |e: &TreeEntry| e.kind != EntryKind::Tree;
                (file(a), &a.name).cmp(&(file(b), &b.name))
            });
            Ok((entries, truncated))
        });
        let ((entries, truncated), last_commit) =
            tokio::try_join!(entries, self.last_commit(oid, path))?;
        Ok(Tree {
            generation: self.view.generation,
            oid: oid.to_string(),
            path: path.to_string(),
            entries,
            truncated,
            last_commit,
        })
    }

    /// The object ID and size of the file at `path`.
    async fn blob_info(&self, oid: &str, path: &str) -> Result<(String, u64), OpError> {
        let commit = object_id(oid)?;
        let path = path.to_string();
        self.read(move |repo, _| {
            let Some(e) = entry_at(repo, commit, &path)? else {
                return Err(OpError::not_found("file"));
            };
            if e.mode().is_tree() {
                return Err(OpError::new("not_a_file", "path is a directory"));
            }
            if e.mode().is_commit() {
                return Err(OpError::not_found("file"));
            }
            let size = repo.find_header(e.object_id())?.size();
            Ok((e.object_id().to_string(), size))
        })
        .await
    }

    /// The contents of a blob.
    async fn read_blob(&self, blob: &str) -> Result<Vec<u8>, OpError> {
        let id = object_id(blob)?;
        self.read(move |repo, _| Ok(repo.find_blob(id)?.take_data()))
            .await
    }

    async fn blob(&self, oid: &str, path: &str) -> Result<Blob, OpError> {
        let path = check_path(path)?;
        if path.is_empty() {
            return Err(OpError::bad("path required"));
        }
        let contents = async {
            let (blob, size) = self.blob_info(oid, path).await?;
            let data = if size <= MAX_BLOB_BYTES {
                Some(self.read_blob(&blob).await?)
            } else {
                None
            };
            Ok::<_, OpError>((blob, size, data))
        };
        let ((blob, size, data), last_commit) =
            tokio::try_join!(contents, self.last_commit(oid, path))?;
        let mut text = None;
        let mut binary = false;
        if let Some(data) = data {
            binary = data[..data.len().min(8000)].contains(&0);
            if !binary {
                match String::from_utf8(data) {
                    Ok(t) => text = Some(t),
                    Err(_) => binary = true,
                }
            }
        }
        Ok(Blob {
            generation: self.view.generation,
            oid: oid.to_string(),
            path: path.to_string(),
            blob,
            size,
            binary,
            too_large: size > MAX_BLOB_BYTES,
            text,
            last_commit,
        })
    }

    async fn commit(&self, oid: &str) -> Result<CommitDetail, OpError> {
        let id = object_id(oid)?;
        let (commit, message) = self
            .read(move |repo, _| {
                let c = repo
                    .try_find_object(id)?
                    .ok_or_else(|| OpError::not_found("commit"))?
                    .try_into_commit()
                    .map_err(|_| OpError::not_found("commit"))?;
                let message = c.message_raw_sloppy().to_str_lossy().trim_end().to_string();
                let mut commit = commit_info(&c)?;
                commit.0.subject = message.lines().next().unwrap_or_default().to_string();
                Ok((commit, message))
            })
            .await?;
        let commit = self
            .link(vec![commit])
            .await
            .pop()
            .map(|commit| FullCommit { commit, message })
            .ok_or_else(|| OpError::not_found("commit"))?;
        // Merges show their changes against the first parent.
        let (diff, truncated) = self
            .run_limited(
                &[
                    "diff-tree",
                    "--patch",
                    "--find-renames",
                    "--root",
                    "--no-commit-id",
                    "--diff-merges=first-parent",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    oid,
                ],
                MAX_DIFF_BYTES,
            )
            .await?;
        Ok(CommitDetail {
            generation: self.view.generation,
            commit,
            diff: String::from_utf8_lossy(&diff).into_owned(),
            truncated,
        })
    }
}
