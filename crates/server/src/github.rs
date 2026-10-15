//! Read-only mirrors of GitHub repositories
//! (docs/decisions/0020-github-mirrors.md).
//!
//! A user connects a GitHub account with the OAuth app of `[github]` and
//! picks repositories to mirror. Each mirror is a repository under the
//! user's name whose catalog record names its GitHub source: no client,
//! merge, or pull request changes it. Every server looks for mirrors that
//! are due ([`Github::run`]); a sync reads GitHub's refs with `ls-remote`,
//! fetches the missing objects into the cache, and changes the refs with the
//! push procedure as a push by the server. Branches and tags follow GitHub
//! exactly, deletions and force pushes included, and so does the default
//! branch.

use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, Semaphore, watch};

use crate::git::hook::{HookCommand, HookResponse};
use crate::http::access::Directory;
use crate::repository::{AccessCheck, Origin, PendingPush, PushAccess, Registry, RepoError};
use worktree_core::catalog::CatalogError;
use worktree_core::config::GithubConfig;
use worktree_core::github::{Due, GithubDb, GithubDbError, Mirror};
use worktree_core::identity::Identity;
use worktree_core::limits;
use worktree_core::model::{Oid, Operation, RefName, RepoState, RequestId};
use worktree_core::repo::{Access, RepoSettings, validate_repo_name};

/// The user of log entries of mirror syncs. No user name contains `:`.
const MIRROR_USER: &str = ":github";
const API: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("worktree/", env!("CARGO_PKG_VERSION"));
/// Repositories listed for picking: ten pages of 100.
const MAX_LIST_PAGES: u32 = 10;

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("sign in first")]
    SignedOut,
    #[error("connect your GitHub account first")]
    NotConnected,
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Exists(String),
    /// GitHub refused the token.
    #[error("GitHub refused access; connect your GitHub account again")]
    Revoked,
    #[error("{0}")]
    Unavailable(String),
}

impl From<GithubDbError> for GithubError {
    fn from(e: GithubDbError) -> Self {
        Self::Unavailable(e.to_string())
    }
}

impl From<reqwest::Error> for GithubError {
    fn from(e: reqwest::Error) -> Self {
        if e.status().is_some_and(|s| s.as_u16() == 401) {
            return Self::Revoked;
        }
        Self::Unavailable(format!("GitHub: {e}"))
    }
}

/// A GitHub repository as the settings page lists it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GithubRepo {
    pub full_name: String,
    pub name: String,
    pub private: bool,
    pub description: Option<String>,
    pub default_branch: String,
    pub pushed_at: Option<String>,
}

#[derive(Deserialize)]
struct GithubUser {
    id: i64,
    login: String,
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: Option<String>,
    error_description: Option<String>,
}

pub struct Github {
    cfg: GithubConfig,
    http: reqwest::Client,
    pub db: GithubDb,
    registry: Arc<Registry>,
    directory: Arc<Directory>,
    /// Wakes [`Github::run`] when a mirror is added.
    wake: Notify,
}

impl Github {
    pub fn new(
        cfg: GithubConfig,
        db: GithubDb,
        registry: Arc<Registry>,
        directory: Arc<Directory>,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            cfg,
            http,
            db,
            registry,
            directory,
            wake: Notify::new(),
        })
    }

    // OAuth and the GitHub API.

    /// The GitHub page that asks the user to grant access. `redirect`: the
    /// callback URL; without it, GitHub uses the app's.
    pub fn authorize_url(&self, state: &str, redirect: Option<&str>) -> String {
        let mut params = vec![
            ("client_id", self.cfg.client_id.as_str()),
            // Private repositories need `repo`; OAuth apps have no
            // read-only scope for them.
            ("scope", "repo"),
            ("state", state),
            ("allow_signup", "false"),
        ];
        if let Some(r) = redirect {
            params.push(("redirect_uri", r));
        }
        reqwest::Url::parse_with_params("https://github.com/login/oauth/authorize", &params)
            .expect("valid URL")
            .to_string()
    }

    /// Exchanges the callback's code for a token and connects the account.
    /// Answers the GitHub login.
    pub async fn finish_connect(&self, user: &str, code: &str) -> Result<String, GithubError> {
        let answer: TokenAnswer = self
            .http
            .post("https://github.com/login/oauth/access_token")
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", self.cfg.client_id.as_str()),
                ("client_secret", self.cfg.client_secret.as_str()),
                ("code", code),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let Some(token) = answer.access_token else {
            return Err(GithubError::Invalid(
                answer
                    .error_description
                    .unwrap_or_else(|| "GitHub did not grant access".into()),
            ));
        };
        let me: GithubUser = self.api(&token, "/user").await?;
        self.db.connect(user, me.id, &me.login, &token).await?;
        self.wake.notify_one();
        tracing::info!(user, github = %me.login, "GitHub account connected");
        Ok(me.login)
    }

    async fn api<T: serde::de::DeserializeOwned>(
        &self,
        token: &str,
        path: &str,
    ) -> Result<T, GithubError> {
        Ok(self
            .http
            .get(format!("{API}{path}"))
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    fn token_of(account: Option<worktree_core::github::Account>) -> Result<String, GithubError> {
        account.map(|a| a.token).ok_or(GithubError::NotConnected)
    }

    // Queries and changes of the settings page.

    /// The account and mirrors of the signed-in user.
    pub async fn status(&self, who: &Identity) -> Result<serde_json::Value, GithubError> {
        let user = signed_in(who)?;
        let account = self.db.account(user).await?;
        let mirrors = self.db.mirrors(user).await?;
        Ok(serde_json::json!({
            "configured": true,
            "account": account,
            "mirrors": mirrors,
        }))
    }

    /// The GitHub repositories the user can read, most recently pushed
    /// first.
    pub async fn repos(&self, who: &Identity) -> Result<Vec<GithubRepo>, GithubError> {
        let user = signed_in(who)?;
        let token = Self::token_of(self.db.account(user).await?)?;
        let mut all = Vec::new();
        for page in 1..=MAX_LIST_PAGES {
            let list: Vec<GithubRepo> = self
                .api(
                    &token,
                    &format!("/user/repos?per_page=100&sort=pushed&page={page}"),
                )
                .await?;
            let last = list.len() < 100;
            all.extend(list);
            if last {
                break;
            }
        }
        Ok(all)
    }

    /// Mirrors a GitHub repository (`owner/name`) as `<user>/<name>` and
    /// starts its first sync. A retry answers the same mirror.
    pub async fn mirror(&self, who: &Identity, full_name: &str) -> Result<Mirror, GithubError> {
        let user = signed_in(who)?;
        let token = Self::token_of(self.db.account(user).await?)?;
        let valid = full_name.split_once('/').is_some_and(|(o, n)| {
            !o.is_empty()
                && !n.is_empty()
                && full_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
                && !n.contains('/')
        });
        if !valid {
            return Err(GithubError::Invalid(format!(
                "invalid GitHub repository {full_name:?}"
            )));
        }
        let r: GithubRepo = match self.api(&token, &format!("/repos/{full_name}")).await {
            Ok(r) => r,
            Err(GithubError::Unavailable(m)) if m.contains("404") => {
                return Err(GithubError::NotFound(format!(
                    "GitHub repository {full_name} not found"
                )));
            }
            Err(e) => return Err(e),
        };
        let name = format!("{user}/{}", r.name);
        validate_repo_name(&name).map_err(|e| GithubError::Invalid(e.to_string()))?;
        let mut settings = RepoSettings::new(&name, worktree_core::model::RepoId::new());
        settings.default_branch = r.default_branch.clone();
        settings.mirror = Some(r.full_name.clone());
        let catalog = self.directory.catalog();
        let id = match catalog.create(&settings, user).await {
            Ok(created) => {
                tracing::info!(repo = %name, github = %r.full_name, user, "mirror created");
                created.id
            }
            Err(CatalogError::Exists(_)) => match catalog.record(&name).await {
                // A retry after the catalog change.
                Ok(Some((rec, _)))
                    if rec.state == RepoState::Active
                        && rec.mirror.as_deref() == Some(r.full_name.as_str()) =>
                {
                    rec.repository
                }
                Ok(_) => return Err(GithubError::Exists(format!("{name} exists already"))),
                Err(e) => return Err(GithubError::Unavailable(e.to_string())),
            },
            Err(CatalogError::Invalid(m)) => return Err(GithubError::Invalid(m)),
            Err(e) => return Err(GithubError::Unavailable(e.to_string())),
        };
        self.db.add(id, &name, user, &r.full_name).await?;
        self.wake.notify_one();
        Ok(Mirror {
            name,
            github: r.full_name,
            synced_ms: None,
            error: None,
        })
    }

    /// Stops syncing a mirror. The repository stays and takes pushes.
    pub async fn unmirror(&self, who: &Identity, name: &str) -> Result<(), GithubError> {
        let user = signed_in(who)?;
        let Some(id) = self.db.find(user, name).await? else {
            return Err(GithubError::NotFound(format!("no mirror {name}")));
        };
        match self
            .directory
            .catalog()
            .update(name, |r| {
                if r.repository == id {
                    r.mirror = None;
                }
            })
            .await
        {
            Ok(_) | Err(CatalogError::NotFound(_)) => {}
            Err(e) => return Err(GithubError::Unavailable(e.to_string())),
        }
        self.db.remove(id).await?;
        tracing::info!(repo = %name, user, "mirror stopped");
        Ok(())
    }

    pub async fn disconnect(&self, who: &Identity) -> Result<(), GithubError> {
        let user = signed_in(who)?;
        self.db.disconnect(user).await?;
        tracing::info!(user, "GitHub account disconnected");
        Ok(())
    }

    // Syncing.

    /// Syncs mirrors that are due until the stop signal.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let slots = Arc::new(Semaphore::new(limits::GITHUB_CONCURRENT_SYNCS));
        let mut tick = tokio::time::interval(limits::GITHUB_POLL_INTERVAL);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.wake.notified() => {}
                _ = crate::server::stopped(&mut stop) => break,
            }
            loop {
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    break;
                };
                let due = match self.db.claim(limits::GITHUB_SYNC_LEASE).await {
                    Ok(Some(d)) => d,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "mirrors not synced");
                        break;
                    }
                };
                let me = self.clone();
                // A sync is not cancelled on stop: before its CAS nothing
                // changes, and after it the push is retained.
                tokio::spawn(async move {
                    me.sync_due(due).await;
                    drop(slot);
                });
            }
        }
    }

    async fn sync_due(&self, due: Due) {
        let result = self.sync(&due).await;
        let error = match &result {
            Ok(Synced::Removed) => return,
            Ok(Synced::Done(n)) => {
                if *n > 0 {
                    tracing::info!(repo = %due.name, github = %due.github, refs = n, "mirror synced");
                }
                None
            }
            Err(m) => {
                tracing::info!(repo = %due.name, github = %due.github, error = %m, "mirror not synced");
                Some(m.as_str())
            }
        };
        if let Err(e) = self
            .db
            .finish(due.repo, error, limits::GITHUB_SYNC_INTERVAL)
            .await
        {
            tracing::warn!(repo = %due.name, error = %e, "mirror sync not recorded");
        }
    }

    /// Brings the mirror to GitHub's refs. Errors are messages for the
    /// user.
    async fn sync(&self, due: &Due) -> Result<Synced, String> {
        let catalog = self.directory.catalog();
        let record = catalog
            .record(&due.name)
            .await
            .map_err(|e| format!("storage unavailable: {e}"))?;
        let current = record.filter(|(r, _)| {
            r.state == RepoState::Active
                && r.repository == due.repo
                && r.mirror.as_deref() == Some(due.github.as_str())
        });
        if current.is_none() {
            // Deleted, or no longer a mirror.
            tracing::info!(repo = %due.name, "mirror gone; not syncing it any more");
            self.db.remove(due.repo).await.map_err(|e| e.to_string())?;
            return Ok(Synced::Removed);
        }
        let token = due
            .token
            .as_deref()
            .ok_or("GitHub is not connected; connect it again in Settings")?;
        let remote = self.ls_remote(token, &due.github).await?;

        let controller = self.registry.controller(due.repo);
        let (cache, view, local) = controller.prepare_change().await.map_err(repo_error)?;
        drop(view);
        let format = cache.format;
        let mut wanted = BTreeMap::new();
        for (name, oid) in &remote.refs {
            let oid = Oid::parse(format, oid).map_err(|e| e.to_string())?;
            wanted.insert(name.clone(), oid);
        }
        let mut commands = Vec::new();
        for (name, new) in &wanted {
            let old = local.refs.get(name);
            if old != Some(new) {
                commands.push((name.clone(), old.cloned(), Some(new.clone())));
            }
        }
        for (name, old) in &local.refs {
            if (name.is_branch() || name.is_tag()) && !wanted.contains_key(name) {
                commands.push((name.clone(), Some(old.clone()), None));
            }
        }

        if !commands.is_empty() {
            let fetch: Vec<&RefName> = commands
                .iter()
                .filter(|(_, _, new)| new.is_some())
                .map(|(n, _, _)| n)
                .collect();
            if !fetch.is_empty() {
                self.fetch(&cache.dir, token, &due.github, &fetch).await?;
                let oids: Vec<Oid> = commands.iter().filter_map(|c| c.2.clone()).collect();
                let types = self
                    .registry
                    .svc
                    .git
                    .object_types(&cache.dir, &oids)
                    .await
                    .map_err(|e| e.to_string())?;
                if oids
                    .iter()
                    .any(|o| types.get(o).cloned().flatten().is_none())
                {
                    return Err("GitHub changed during the sync; it is tried again shortly".into());
                }
            }
            let op = PendingPush {
                repo: due.repo,
                user: MIRROR_USER.to_string(),
                request: RequestId::new(),
                access: self.access(due),
                cache: cache.clone(),
                default_branch_changed: None,
                origin: Origin::Server(Operation::Push),
                ref_guards: vec![],
            };
            let hook_commands = commands
                .iter()
                .map(|(name, old, new)| HookCommand {
                    name: name.to_string(),
                    old: Oid::to_git(old.as_ref(), format),
                    new: Oid::to_git(new.as_ref(), format),
                })
                .collect();
            if let Some(why) = failure(&controller.push(&op, hook_commands).await) {
                return Err(why);
            }
        }

        // GitHub's default branch, once it exists here.
        if let Some(head) = &remote.head
            && wanted.contains_key(head)
            && local.head.as_ref() != Some(head)
            && let Some(short) = head.as_str().strip_prefix("refs/heads/")
        {
            catalog
                .set_default_branch(&due.name, short, MIRROR_USER)
                .await
                .map_err(|e| format!("default branch not changed: {e}"))?;
        }
        Ok(Synced::Done(commands.len()))
    }

    /// The push may change the refs while the name still maps to this
    /// mirror of the same GitHub repository.
    fn access(&self, due: &Due) -> AccessCheck {
        let directory = self.directory.clone();
        let (name, id, github) = (due.name.clone(), due.repo, due.github.clone());
        Arc::new(move || {
            let (directory, name, github) = (directory.clone(), name.clone(), github.clone());
            Box::pin(async move {
                let repo = directory.repo(&name).await.map_err(|e| e.to_string())?;
                Ok(match repo {
                    Some(r) if r.id == id && r.mirror.as_deref() == Some(github.as_str()) => {
                        PushAccess {
                            repo: id,
                            level: Access::Force,
                            ref_rules: vec![],
                        }
                    }
                    _ => PushAccess::none(),
                })
            })
        })
    }

    /// A Git command that talks to GitHub with the token. The token goes
    /// into the environment, never into arguments or URLs, so it stays out
    /// of process lists and messages. Only HTTPS is permitted.
    fn remote_command(
        &self,
        dir: Option<&std::path::Path>,
        token: &str,
    ) -> tokio::process::Command {
        let git = &self.registry.svc.git;
        let mut c = match dir {
            Some(d) => git.in_repo(d),
            None => git.command(),
        };
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        for (i, (k, v)) in [
            (
                "http.https://github.com/.extraHeader",
                format!("Authorization: Basic {basic}"),
            ),
            ("credential.helper", String::new()),
            ("protocol.allow", "never".into()),
            ("protocol.https.allow", "always".into()),
        ]
        .into_iter()
        .enumerate()
        {
            c.env(format!("GIT_CONFIG_KEY_{i}"), k)
                .env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
        c.env("GIT_CONFIG_COUNT", "4");
        c
    }

    async fn ls_remote(&self, token: &str, github: &str) -> Result<Remote, String> {
        let mut c = self.remote_command(None, token);
        c.args(["ls-remote", "--symref", &url(github)]).args([
            "HEAD",
            "refs/heads/*",
            "refs/tags/*",
        ]);
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            crate::git::run(&mut c, None),
        )
        .await
        .map_err(|_| "GitHub did not answer in time".to_string())?
        .map_err(git_message)?;
        let mut remote = Remote::default();
        for line in String::from_utf8_lossy(&out).lines() {
            let Some((left, name)) = line.split_once('\t') else {
                continue;
            };
            if let Some(target) = left.strip_prefix("ref: ") {
                if name == "HEAD" {
                    remote.head = RefName::parse(target).ok();
                }
                continue;
            }
            // Peeled tags, HEAD itself, and names this server refuses.
            if name.ends_with("^{}") {
                continue;
            }
            let Ok(r) = RefName::parse(name) else {
                continue;
            };
            if r.is_branch() || r.is_tag() {
                remote.refs.insert(r, left.to_string());
            }
        }
        Ok(remote)
    }

    /// Fetches the objects of `refs` into the cache without changing its
    /// refs; the push procedure changes them.
    async fn fetch(
        &self,
        dir: &std::path::Path,
        token: &str,
        github: &str,
        refs: &[&RefName],
    ) -> Result<(), String> {
        let mut c = self.remote_command(Some(dir), token);
        c.args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-auto-gc",
            "--no-auto-maintenance",
            "--no-recurse-submodules",
            "--stdin",
            &url(github),
        ]);
        // Refspecs without a destination store nothing.
        let input: String = refs.iter().map(|r| format!("{r}\n")).collect();
        tokio::time::timeout(
            limits::GITHUB_FETCH_TIMEOUT,
            crate::git::run(&mut c, Some(input.into_bytes())),
        )
        .await
        .map_err(|_| "fetching from GitHub took too long".to_string())?
        .map_err(git_message)?;
        Ok(())
    }
}

enum Synced {
    /// The number of changed refs.
    Done(usize),
    /// The mirror no longer exists.
    Removed,
}

#[derive(Default)]
struct Remote {
    head: Option<RefName>,
    /// Branches and tags with their object IDs.
    refs: BTreeMap<RefName, String>,
}

fn url(github: &str) -> String {
    format!("https://github.com/{github}.git")
}

fn signed_in(who: &Identity) -> Result<&str, GithubError> {
    match who {
        Identity::Anonymous => Err(GithubError::SignedOut),
        Identity::User { name, .. } => Ok(name),
    }
}

/// A short message for a failed Git command against GitHub.
fn git_message(e: crate::git::GitError) -> String {
    let text = e.to_string();
    if text.contains("Authentication failed")
        || text.contains("could not read Username")
        || text.contains("returned error: 401")
        || text.contains("returned error: 403")
    {
        "GitHub refused access; connect your GitHub account again in Settings".into()
    } else if text.contains("not found") {
        "the repository is gone from GitHub, or your account cannot read it any more".into()
    } else {
        text.chars().take(500).collect()
    }
}

fn repo_error(e: RepoError) -> String {
    match e {
        RepoError::NotFound | RepoError::Deleted => "repository not found".into(),
        e => e.to_string(),
    }
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
