//! Read queries of the web interface: repositories, refs, history, trees,
//! files, and commits (docs/decisions/0014-web-interface.md). Queries of
//! pull requests are in [`super::pulls`].
//!
//! Each query reads a [`ReadView`](crate::repository::ReadView), so it sees
//! every push accepted before it, and answers with the view's generation.
//! The cache also holds objects of rejected pushes: a query names a branch,
//! a tag, or a commit that a ref reaches, never any other object.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::http::AppInner;
use crate::repository::RepoError;
use worktree_core::catalog::CatalogError;
use worktree_core::identity::Identity;
use worktree_core::model::RefName;
use worktree_core::repo::{Access, RepoSettings};

/// Files above this size are shown without contents.
const MAX_BLOB_BYTES: u64 = 1024 * 1024;
/// The raw route refuses larger files.
const MAX_RAW_BYTES: u64 = 20 * 1024 * 1024;
pub(super) const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_TREE_ENTRIES: usize = 5000;
const MAX_LOG: u32 = 100;
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Repos,
    /// Creates an empty repository; needs a session.
    RepoCreate {
        name: String,
        /// Anyone may read it, also without signing in.
        #[serde(default)]
        public: bool,
    },
    Refs {
        repo: String,
    },
    Log {
        repo: String,
        rev: String,
        #[serde(default)]
        path: String,
        #[serde(default)]
        skip: u32,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    Tree {
        repo: String,
        rev: String,
        #[serde(default)]
        path: String,
    },
    Blob {
        repo: String,
        rev: String,
        path: String,
    },
    Commit {
        repo: String,
        rev: String,
    },
    // Pull requests (docs/decisions/0019-pull-requests.md), answered by
    // `super::pulls`. The ones that change something need a session.
    Pulls {
        repo: String,
        /// Closed and merged ones instead of open ones.
        #[serde(default)]
        closed: bool,
        #[serde(default)]
        skip: u32,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    Pull {
        repo: String,
        number: u64,
    },
    /// The commits and changes of `head` since its merge base with `base`.
    Compare {
        repo: String,
        base: String,
        head: String,
    },
    PullCreate {
        repo: String,
        source: String,
        target: String,
        title: String,
        #[serde(default)]
        body: String,
        /// A UUID: a retry opens no second pull request.
        #[serde(default)]
        request: Option<String>,
    },
    PullComment {
        repo: String,
        number: u64,
        body: String,
        #[serde(default)]
        request: Option<String>,
    },
    PullClose {
        repo: String,
        number: u64,
    },
    PullReopen {
        repo: String,
        number: u64,
    },
    PullMerge {
        repo: String,
        number: u64,
        /// The source commit the page showed.
        head: String,
        #[serde(default)]
        squash: bool,
        request: String,
    },
}

fn default_limit() -> u32 {
    30
}

#[derive(Debug)]
pub struct ReadError {
    pub code: &'static str,
    pub message: String,
}

impl ReadError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(super) fn not_found(what: &str) -> Self {
        Self::new("not_found", format!("{what} not found"))
    }
    pub(super) fn bad(message: impl Into<String>) -> Self {
        Self::new("bad_request", message)
    }
}

impl From<RepoError> for ReadError {
    fn from(e: RepoError) -> Self {
        match e {
            RepoError::NotFound | RepoError::Deleted => Self::not_found("repository"),
            RepoError::Unavailable(m) => {
                tracing::warn!(error = %m, "storage unavailable");
                Self::new("unavailable", "storage unavailable; retry later")
            }
            RepoError::NoSpace(_) => Self::new("unavailable", "server busy; retry later"),
            RepoError::Internal(m) => {
                tracing::error!(error = %m, "internal error");
                Self::new("internal", "internal error")
            }
        }
    }
}

impl From<crate::git::GitError> for ReadError {
    fn from(e: crate::git::GitError) -> Self {
        RepoError::from(e).into()
    }
}

impl From<std::io::Error> for ReadError {
    fn from(e: std::io::Error) -> Self {
        RepoError::from(e).into()
    }
}

/// The repository for a name if `who` may read it. Unknown repositories
/// and missing permission look the same.
pub async fn readable(s: &AppInner, who: &Identity, name: &str) -> Result<RepoSettings, ReadError> {
    if worktree_core::repo::validate_repo_name(name).is_err() {
        return Err(ReadError::not_found("repository"));
    }
    let repo = s
        .directory
        .repo(name)
        .await
        .map_err(|e| ReadError::from(RepoError::from(e)))?
        .ok_or_else(|| ReadError::not_found("repository"))?;
    if s.directory.access(who, &repo) < Access::Read {
        return Err(ReadError::not_found("repository"));
    }
    Ok(repo)
}

pub async fn run(s: &AppInner, who: &Identity, q: Query) -> Result<Value, ReadError> {
    let name = match &q {
        Query::Repos => return repos(s, who).await,
        Query::RepoCreate { name, public } => return create(s, who, name, *public).await,
        Query::Refs { repo }
        | Query::Log { repo, .. }
        | Query::Tree { repo, .. }
        | Query::Blob { repo, .. }
        | Query::Commit { repo, .. }
        | Query::Pulls { repo, .. }
        | Query::Pull { repo, .. }
        | Query::Compare { repo, .. }
        | Query::PullCreate { repo, .. }
        | Query::PullComment { repo, .. }
        | Query::PullClose { repo, .. }
        | Query::PullReopen { repo, .. }
        | Query::PullMerge { repo, .. } => repo.clone(),
    };
    let repo = readable(s, who, &name).await?;
    if !matches!(
        q,
        Query::Refs { .. }
            | Query::Log { .. }
            | Query::Tree { .. }
            | Query::Blob { .. }
            | Query::Commit { .. }
    ) {
        return super::pulls::run(s, who, &repo, q).await;
    }
    let view = s.registry.controller(repo.id).prepare_read().await?;
    let _permits = crate::http::process_permit(s)
        .await
        .map_err(|_| ReadError::new("busy", "server busy; retry later"))?;
    let g = Git { s, dir: &view.dir };
    let mut out = match q {
        Query::Repos
        | Query::RepoCreate { .. }
        | Query::Pulls { .. }
        | Query::Pull { .. }
        | Query::Compare { .. }
        | Query::PullCreate { .. }
        | Query::PullComment { .. }
        | Query::PullClose { .. }
        | Query::PullReopen { .. }
        | Query::PullMerge { .. } => unreachable!(),
        Query::Refs { .. } => g.refs().await?,
        Query::Log {
            rev,
            path,
            skip,
            limit,
            ..
        } => {
            let oid = g.resolve(&rev).await?;
            g.log(&oid, &path, skip, limit.clamp(1, MAX_LOG)).await?
        }
        Query::Tree { rev, path, .. } => {
            let oid = g.resolve(&rev).await?;
            g.tree(&oid, &path).await?
        }
        Query::Blob { rev, path, .. } => {
            let oid = g.resolve(&rev).await?;
            g.blob(&oid, &path).await?
        }
        Query::Commit { rev, .. } => {
            let oid = g.resolve(&rev).await?;
            g.commit(&oid).await?
        }
    };
    out["generation"] = json!(view.generation);
    Ok(out)
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
) -> Result<RawFile, ReadError> {
    let repo = readable(s, who, name).await?;
    let view = s.registry.controller(repo.id).prepare_read().await?;
    let _permits = crate::http::process_permit(s)
        .await
        .map_err(|_| ReadError::new("busy", "server busy; retry later"))?;
    let g = Git { s, dir: &view.dir };
    let (rev, path) = g.split_rev(rest).await?;
    let oid = g.resolve(rev).await?;
    let path = check_path(path)?;
    if path.is_empty() {
        return Err(ReadError::bad("path required"));
    }
    let (blob, size) = g.blob_info(&oid, path).await?;
    let data = if cached == Some(blob.as_str()) {
        None
    } else if size > MAX_RAW_BYTES {
        return Err(ReadError::new("too_large", "file too large"));
    } else {
        Some(g.run(&["cat-file", "blob", &blob], None).await?)
    };
    Ok(RawFile {
        path: path.to_string(),
        blob,
        data,
    })
}

#[derive(Serialize)]
struct RepoSummary {
    name: String,
    default_branch: String,
    anonymous_read: bool,
}

async fn repos(s: &AppInner, who: &Identity) -> Result<Value, ReadError> {
    let all: Vec<RepoSettings> = s
        .directory
        .catalog()
        .list()
        .await
        .map_err(|e| ReadError::from(RepoError::from(e)))?
        .into_iter()
        .filter(|r| r.state == worktree_core::model::RepoState::Active)
        .map(|r| r.settings())
        .collect();
    let repos: Vec<RepoSummary> = all
        .into_iter()
        .filter(|r| s.directory.access(who, r) >= Access::Read)
        .map(|r| RepoSummary {
            name: r.name,
            default_branch: r.default_branch,
            anonymous_read: r.anonymous_read,
        })
        .collect();
    Ok(json!({ "repos": repos }))
}

/// Creates a repository as a push to a missing name would
/// (docs/decisions/0005-create-on-push.md), with the chosen visibility.
async fn create(
    s: &AppInner,
    who: &Identity,
    name: &str,
    public: bool,
) -> Result<Value, ReadError> {
    if *who == Identity::Anonymous {
        return Err(ReadError::new("unauthorized", "sign in first"));
    }
    worktree_core::repo::validate_repo_name(name).map_err(|e| ReadError::bad(e.to_string()))?;
    if !s.directory.may_create_on_push(who, name) {
        return Err(ReadError::new(
            "forbidden",
            format!("you cannot create repositories under {name}"),
        ));
    }
    let mut settings = RepoSettings::new(name, worktree_core::model::RepoId::new());
    settings.anonymous_read = public;
    let repo = match s.directory.catalog().create(&settings, who.name()).await {
        Ok(r) => r,
        Err(CatalogError::Exists(_)) => {
            return Err(ReadError::new("exists", format!("{name} exists already")));
        }
        Err(CatalogError::Invalid(m)) => return Err(ReadError::bad(m)),
        Err(CatalogError::Storage(e)) => return Err(RepoError::from(e).into()),
        Err(e) => {
            tracing::warn!(error = %e, "repository catalog unavailable");
            return Err(ReadError::new(
                "unavailable",
                "repository catalog unavailable; retry later",
            ));
        }
    };
    s.registry.svc.metrics.inc("repositories_created_on_web");
    tracing::info!(repo = %name, id = %repo.id, user = %who.name(), public, "created repository on the web");
    Ok(json!({
        "repo": RepoSummary {
            name: repo.name,
            default_branch: repo.default_branch,
            anonymous_read: repo.anonymous_read,
        }
    }))
}

/// Commit fields for `--format`, separated by NUL; records end with RS.
pub(super) const COMMIT_FORMAT: &str =
    "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%s%x1e";

pub(super) fn parse_commits(out: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(out)
        .split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            let f: Vec<&str> = rec.splitn(9, '\0').collect();
            let [oid, parents, an, ae, at, cn, ce, ct, subject] = f[..] else {
                return None;
            };
            Some(json!({
                "oid": oid,
                "parents": parents.split_whitespace().collect::<Vec<_>>(),
                "author": { "name": an, "email": ae, "time": at.parse::<i64>().unwrap_or(0) },
                "committer": { "name": cn, "email": ce, "time": ct.parse::<i64>().unwrap_or(0) },
                "subject": subject,
            }))
        })
        .collect()
}

/// A path inside a tree: `/`-separated names, no `.` or `..`.
fn check_path(path: &str) -> Result<&str, ReadError> {
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
        Err(ReadError::bad("invalid path"))
    }
}

pub(super) struct Git<'a> {
    pub(super) s: &'a AppInner,
    pub(super) dir: &'a Path,
}

impl Git<'_> {
    fn cmd(&self) -> Command {
        let mut c = self.s.registry.svc.git.service_command();
        c.env("GIT_DIR", self.dir);
        c
    }

    pub(super) async fn run(
        &self,
        args: &[&str],
        input: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, ReadError> {
        let mut c = self.cmd();
        c.args(args);
        match tokio::time::timeout(GIT_TIMEOUT, crate::git::run(&mut c, input)).await {
            Ok(r) => Ok(r?),
            Err(_) => Err(ReadError::new("unavailable", "query took too long")),
        }
    }

    /// Like [`run`](Self::run), but keeps at most `max` bytes of output and
    /// stops Git when there is more.
    pub(super) async fn run_limited(
        &self,
        args: &[&str],
        max: usize,
    ) -> Result<(Vec<u8>, bool), ReadError> {
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
            Err(_) => return Err(ReadError::new("unavailable", "query took too long")),
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

    /// Whether `spec` names a commit; answers its object ID.
    pub(super) async fn commit_oid(&self, spec: &str) -> Option<String> {
        let spec = format!("{spec}^{{commit}}");
        let out = self
            .run(
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &spec,
                ],
                None,
            )
            .await
            .ok()?;
        Some(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// The commit for a branch name, a tag name, a full ref name, or the
    /// object ID of a commit that a ref reaches.
    pub(super) async fn resolve(&self, rev: &str) -> Result<String, ReadError> {
        let is_oid =
            (rev.len() == 40 || rev.len() == 64) && rev.bytes().all(|b| b.is_ascii_hexdigit());
        if is_oid {
            let rev = rev.to_ascii_lowercase();
            let Some(oid) = self.commit_oid(&rev).await else {
                return Err(ReadError::not_found("commit"));
            };
            let reached = self
                .run(
                    &[
                        "for-each-ref",
                        "--count=1",
                        "--format=x",
                        "--contains",
                        &oid,
                    ],
                    None,
                )
                .await?;
            return if reached.is_empty() {
                Err(ReadError::not_found("commit"))
            } else {
                Ok(oid)
            };
        }
        let candidates: Vec<String> = if rev.starts_with("refs/") {
            vec![rev.to_string()]
        } else {
            vec![format!("refs/heads/{rev}"), format!("refs/tags/{rev}")]
        };
        for c in candidates {
            if RefName::parse(&c).is_err() {
                return Err(ReadError::bad("invalid ref name"));
            }
            if let Some(oid) = self.commit_oid(&c).await {
                return Ok(oid);
            }
        }
        Err(ReadError::not_found("branch or tag"))
    }

    /// Splits `<rev>/<path>` at the longest branch or tag name, else after
    /// the first segment.
    async fn split_rev<'r>(&self, rest: &'r str) -> Result<(&'r str, &'r str), ReadError> {
        let first = rest.split_once('/').unwrap_or((rest, ""));
        if !first.1.contains('/') {
            return Ok(first);
        }
        let out = self
            .run(
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/heads/",
                    "refs/tags/",
                ],
                None,
            )
            .await?;
        let out = String::from_utf8_lossy(&out);
        let names: std::collections::HashSet<&str> = out
            .lines()
            .filter_map(|l| {
                l.strip_prefix("refs/heads/")
                    .or_else(|| l.strip_prefix("refs/tags/"))
            })
            .collect();
        let longest = rest
            .rmatch_indices('/')
            .map(|(i, _)| i)
            .find(|&i| names.contains(&rest[..i]));
        Ok(longest.map_or(first, |i| (&rest[..i], &rest[i + 1..])))
    }

    /// The best common ancestor of two commits; `None` without one.
    pub(super) async fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>, ReadError> {
        let out = self
            .cmd()
            .args(["merge-base", a, b])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        let out = match tokio::time::timeout(GIT_TIMEOUT, out).await {
            Ok(r) => r?,
            Err(_) => return Err(ReadError::new("unavailable", "query took too long")),
        };
        match out.status.code() {
            Some(0) => Ok(Some(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )),
            Some(1) => Ok(None),
            _ => Err(RepoError::Internal("git merge-base failed".into()).into()),
        }
    }

    async fn refs(&self) -> Result<Value, ReadError> {
        let out = self
            .run(
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)%00%(*objectname)%00%(creatordate:unix)%00%(subject)",
                    "refs/heads/",
                    "refs/tags/",
                ],
                None,
            )
            .await?;
        let (mut branches, mut tags) = (vec![], vec![]);
        for line in String::from_utf8_lossy(&out).lines() {
            let f: Vec<&str> = line.splitn(5, '\0').collect();
            let [name, oid, peeled, time, subject] = f[..] else {
                continue;
            };
            let entry = |short: &str| {
                json!({
                    "name": short,
                    // The commit of an annotated tag.
                    "oid": if peeled.is_empty() { oid } else { peeled },
                    "time": time.parse::<i64>().unwrap_or(0),
                    "subject": subject,
                })
            };
            if let Some(b) = name.strip_prefix("refs/heads/") {
                branches.push(entry(b));
            } else if let Some(t) = name.strip_prefix("refs/tags/") {
                tags.push(entry(t));
            }
        }
        let head = self.s.registry.svc.git.head_target(self.dir).await?;
        Ok(json!({
            "head": head.strip_prefix("refs/heads/").unwrap_or(&head),
            "branches": branches,
            "tags": tags,
        }))
    }

    async fn log(&self, oid: &str, path: &str, skip: u32, limit: u32) -> Result<Value, ReadError> {
        let path = check_path(path)?;
        let skip = format!("--skip={skip}");
        let count = format!("--max-count={}", limit + 1);
        let mut args = vec!["log", COMMIT_FORMAT, &skip, &count, oid, "--"];
        if !path.is_empty() {
            args.push(path);
        }
        let out = self.run(&args, None).await?;
        let mut commits = parse_commits(&out);
        let more = commits.len() > limit as usize;
        commits.truncate(limit as usize);
        Ok(json!({ "oid": oid, "commits": commits, "more": more }))
    }

    async fn last_commit(&self, oid: &str, path: &str) -> Result<Value, ReadError> {
        let mut args = vec!["log", COMMIT_FORMAT, "--max-count=1", oid, "--"];
        if !path.is_empty() {
            args.push(path);
        }
        let out = self.run(&args, None).await?;
        Ok(parse_commits(&out).pop().unwrap_or(Value::Null))
    }

    async fn tree(&self, oid: &str, path: &str) -> Result<Value, ReadError> {
        let path = check_path(path)?;
        let spec = if path.is_empty() {
            format!("{oid}^{{tree}}")
        } else {
            format!("{oid}:{path}")
        };
        let kind = self.object_type(&spec).await?;
        match kind.as_deref() {
            Some("tree") => {}
            Some(_) => return Err(ReadError::new("not_a_tree", "path is a file")),
            None => return Err(ReadError::not_found("path")),
        }
        let out = self
            .run(
                &["ls-tree", "-z", "--long", "--end-of-options", &spec],
                None,
            )
            .await?;
        let mut entries = vec![];
        let mut truncated = false;
        for rec in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            if entries.len() == MAX_TREE_ENTRIES {
                truncated = true;
                break;
            }
            let rec = String::from_utf8_lossy(rec);
            let Some((meta, name)) = rec.split_once('\t') else {
                continue;
            };
            let m: Vec<&str> = meta.split_whitespace().collect();
            let [mode, kind, oid, size] = m[..] else {
                continue;
            };
            let kind = match (kind, mode) {
                ("tree", _) => "tree",
                ("commit", _) => "submodule",
                (_, "120000") => "symlink",
                _ => "file",
            };
            entries.push(json!({
                "name": name,
                "kind": kind,
                "mode": mode,
                "oid": oid,
                "size": size.parse::<u64>().ok(),
            }));
        }
        entries.sort_by(|a, b| {
            let dir = |v: &Value| v["kind"] != "tree";
            (dir(a), a["name"].as_str()).cmp(&(dir(b), b["name"].as_str()))
        });
        Ok(json!({
            "oid": oid,
            "path": path,
            "entries": entries,
            "truncated": truncated,
            "last_commit": self.last_commit(oid, path).await?,
        }))
    }

    async fn object_type(&self, spec: &str) -> Result<Option<String>, ReadError> {
        let out = self
            .run(
                &["cat-file", "--batch-check=%(objecttype)"],
                Some(format!("{spec}\n").into_bytes()),
            )
            .await?;
        let s = String::from_utf8_lossy(&out).trim().to_string();
        Ok((!s.ends_with("missing") && !s.contains(' ')).then_some(s))
    }

    /// The object ID and size of the file at `path`.
    async fn blob_info(&self, oid: &str, path: &str) -> Result<(String, u64), ReadError> {
        let spec = format!("{oid}:{path}");
        let out = self
            .run(
                &[
                    "cat-file",
                    "--batch-check=%(objectname) %(objecttype) %(objectsize)",
                ],
                Some(format!("{spec}\n").into_bytes()),
            )
            .await?;
        let line = String::from_utf8_lossy(&out).trim().to_string();
        let f: Vec<&str> = line.split(' ').collect();
        let [blob, kind, size] = f[..] else {
            return Err(ReadError::not_found("file"));
        };
        if kind != "blob" {
            return Err(ReadError::new("not_a_file", "path is a directory"));
        }
        Ok((blob.to_string(), size.parse().unwrap_or(u64::MAX)))
    }

    async fn blob(&self, oid: &str, path: &str) -> Result<Value, ReadError> {
        let path = check_path(path)?;
        if path.is_empty() {
            return Err(ReadError::bad("path required"));
        }
        let (blob, size) = self.blob_info(oid, path).await?;
        let blob = blob.as_str();
        let mut text = None;
        let mut binary = false;
        if size <= MAX_BLOB_BYTES {
            let data = self.run(&["cat-file", "blob", blob], None).await?;
            binary = data[..data.len().min(8000)].contains(&0);
            if !binary {
                match String::from_utf8(data) {
                    Ok(t) => text = Some(t),
                    Err(_) => binary = true,
                }
            }
        }
        Ok(json!({
            "oid": oid,
            "path": path,
            "blob": blob,
            "size": size,
            "binary": binary,
            "too_large": size > MAX_BLOB_BYTES,
            "text": text,
            "last_commit": self.last_commit(oid, path).await?,
        }))
    }

    async fn commit(&self, oid: &str) -> Result<Value, ReadError> {
        let out = self
            .run(
                &[
                    "show",
                    "--no-patch",
                    "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%B",
                    oid,
                ],
                None,
            )
            .await?;
        let mut commit = parse_commits(&[out.as_slice(), b"\x1e"].concat())
            .pop()
            .ok_or_else(|| ReadError::not_found("commit"))?;
        let message = commit["subject"]
            .as_str()
            .unwrap_or_default()
            .trim_end()
            .to_string();
        commit["message"] = json!(message);
        commit["subject"] = json!(message.lines().next().unwrap_or_default());
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
        Ok(json!({
            "commit": commit,
            "diff": String::from_utf8_lossy(&diff),
            "truncated": truncated,
        }))
    }
}
