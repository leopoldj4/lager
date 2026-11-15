//! The operations of the web interface: the queries and changes that pages
//! send over the live connection (docs/decisions/0014-web-interface.md).
//!
//! Each operation is a type with the fields of its request, in the module
//! that answers it; [`Op`] names the type of its answer. The table below
//! lists them all by name. The TypeScript types of the web interface are
//! written from the same table
//! (docs/decisions/0035-generated-wire-types.md).

use std::any::TypeId;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use ts_rs::{Config, TS, TypeVisitor};

use super::{github, profile, pulls, read, repos};
use crate::http::AppInner;
use crate::repository::RepoError;
use lager_core::identity::Identity;
use lager_core::repo::{Access, RepoSettings};

/// An operation: the fields of a `req` message without `id` and `op`.
pub trait Op: DeserializeOwned + TS + 'static {
    type Answer: Serialize + TS + 'static;

    fn run(
        self,
        s: &AppInner,
        who: &Identity,
    ) -> impl Future<Output = Result<Self::Answer, OpError>> + Send;
}

/// Writes [`run`] and [`typescript`] from the operations by name.
macro_rules! ops {
    ($($name:literal => $op:ty,)*) => {
        /// Answers the operation `name` with the fields `args`.
        pub async fn run(
            s: &AppInner,
            who: &Identity,
            name: &str,
            args: Value,
        ) -> Result<Value, OpError> {
            match name {
                $($name => answer::<$op>(s, who, args).await,)*
                _ => Err(OpError::bad(format!("unknown operation {name}"))),
            }
        }

        /// The TypeScript types of the answers, and `Ops`: the fields and
        /// the answer of every operation by name.
        pub fn typescript() -> String {
            let mut types = Types::default();
            $(types.op::<$op>($name);)*
            types.finish()
        }
    };
}

ops! {
    "repos" => repos::ReposOp,
    "repo_create" => repos::RepoCreateOp,
    "repo_rename" => repos::RepoRenameOp,
    "repo_visibility" => repos::RepoVisibilityOp,
    "repo_delete" => repos::RepoDeleteOp,
    "refs" => read::RefsOp,
    "log" => read::LogOp,
    "tree" => read::TreeOp,
    "blob" => read::BlobOp,
    "commit" => read::CommitOp,
    "pulls" => pulls::PullsOp,
    "pull" => pulls::PullOp,
    "compare" => pulls::CompareOp,
    "pull_create" => pulls::PullCreateOp,
    "pull_comment" => pulls::PullCommentOp,
    "pull_close" => pulls::PullCloseOp,
    "pull_reopen" => pulls::PullReopenOp,
    "pull_merge" => pulls::PullMergeOp,
    "profile" => profile::ProfileOp,
    "github" => github::GithubOp,
    "github_repos" => github::GithubReposOp,
    "github_mirror" => github::GithubMirrorOp,
    "github_unmirror" => github::GithubUnmirrorOp,
    "github_disconnect" => github::GithubDisconnectOp,
    "github_auto" => github::GithubAutoOp,
    "github_unlink" => github::GithubUnlinkOp,
}

async fn answer<O: Op>(s: &AppInner, who: &Identity, args: Value) -> Result<Value, OpError> {
    let op: O = serde_json::from_value(args).map_err(|e| OpError::bad(e.to_string()))?;
    let out = op.run(s, who).await?;
    serde_json::to_value(out).map_err(|e| RepoError::Internal(format!("answer: {e}")).into())
}

/// The answer of a change that has nothing to tell.
#[derive(Serialize, TS)]
pub struct Empty {}

/// Collects the declarations of the answers and of the types in them, each
/// once, and the entries of `Ops`.
struct Types {
    cfg: Config,
    seen: HashSet<TypeId>,
    /// By name: the order of the visits changes from build to build.
    decls: BTreeMap<String, String>,
    ops: Vec<String>,
}

impl Default for Types {
    fn default() -> Self {
        Self {
            // JSON numbers; no count or time here leaves their exact range.
            cfg: Config::new().with_large_int("number"),
            seen: HashSet::new(),
            decls: BTreeMap::new(),
            ops: vec![],
        }
    }
}

impl TypeVisitor for Types {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        // Only types with a name have a declaration.
        if T::output_path().is_none() || !self.seen.insert(TypeId::of::<T>()) {
            return;
        }
        T::visit_dependencies(self);
        let docs = T::docs().unwrap_or_default();
        self.decls.insert(
            T::name(&self.cfg),
            format!("{docs}export {}", T::decl(&self.cfg)),
        );
    }
}

impl Types {
    fn op<O: Op>(&mut self, name: &str) {
        O::visit_dependencies(self);
        self.visit::<O::Answer>();
        self.ops.push(format!(
            "  {name}: {{ args: {}; answer: {} }}",
            O::inline(&self.cfg),
            O::Answer::name(&self.cfg)
        ));
    }

    fn finish(self) -> String {
        format!(
            "// Written by scripts/wire.ts from the types of the server's operations\n\
             // (crates/server/src/web/ops.rs); do not edit.\n\n{}\n\n\
             /** The fields and the answer of every operation by name. */\n\
             export type Ops = {{\n{}\n}}\n",
            Vec::from_iter(self.decls.into_values()).join("\n\n"),
            self.ops.join("\n"),
        )
    }
}

/// Entries of a list when the request names no `limit`.
pub(super) const DEFAULT_LIMIT: u32 = 30;

#[derive(Debug)]
pub struct OpError {
    pub code: &'static str,
    pub message: String,
}

impl OpError {
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

impl From<RepoError> for OpError {
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

impl From<crate::git::GitError> for OpError {
    fn from(e: crate::git::GitError) -> Self {
        RepoError::from(e).into()
    }
}

impl From<std::io::Error> for OpError {
    fn from(e: std::io::Error) -> Self {
        RepoError::from(e).into()
    }
}

/// The repository for a name if `who` may read it. Unknown repositories
/// and missing permission look the same.
pub async fn readable(s: &AppInner, who: &Identity, name: &str) -> Result<RepoSettings, OpError> {
    if lager_core::repo::validate_repo_name(name).is_err() {
        return Err(OpError::not_found("repository"));
    }
    let repo = s
        .directory
        .repo(name)
        .await
        .map_err(|e| OpError::from(RepoError::from(e)))?
        .ok_or_else(|| OpError::not_found("repository"))?;
    if s.directory.access(who, &repo) < Access::Read {
        return Err(OpError::not_found("repository"));
    }
    Ok(repo)
}
