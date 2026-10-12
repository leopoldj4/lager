//! Identity checks, repository names, and permissions.
//!
//! Identities come from an [`IdentityProvider`] (`worktree_core::identity`).
//! Repository names come from the configuration file or from the repository
//! catalog in storage. The directory reloads the file when it changes; each
//! reload increases the permission-rule version that log entries record.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use base64::Engine;

use worktree_core::catalog::Catalog;
use worktree_core::config::{Access, Config, RepoConfig};
pub use worktree_core::identity::{AuthError, Identity};
use worktree_core::identity::{IdentityProvider, NamedUser};
use worktree_core::storage::StorageError;

pub struct Directory {
    inner: RwLock<Inner>,
    path: Option<PathBuf>,
    mtime: Mutex<Option<SystemTime>>,
    identities: Arc<dyn IdentityProvider>,
    catalog: Option<Arc<Catalog>>,
}

struct Inner {
    version: u64,
    repos: HashMap<String, RepoConfig>,
    create_on_push: Option<Access>,
}

impl Directory {
    pub fn new(cfg: &Config, path: Option<PathBuf>, identities: Arc<dyn IdentityProvider>) -> Self {
        let mtime = path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        Self {
            inner: RwLock::new(Inner::from(cfg, 1)),
            path,
            mtime: Mutex::new(mtime),
            identities,
            catalog: None,
        }
    }

    /// Takes repository names from the catalog instead of the file.
    pub fn with_catalog(mut self, catalog: Arc<Catalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    pub fn catalog(&self) -> Option<&Arc<Catalog>> {
        self.catalog.as_ref()
    }

    /// Reloads the table if the configuration file changed. A file that does
    /// not load keeps the previous table.
    pub fn reload_if_changed(&self) {
        let Some(path) = &self.path else { return };
        let Ok(m) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            return;
        };
        let mut last = self.mtime.lock().unwrap();
        if *last == Some(m) {
            return;
        }
        *last = Some(m);
        match Config::load(path) {
            Ok(cfg) => {
                let mut inner = self.inner.write().unwrap();
                let v = inner.version + 1;
                *inner = Inner::from(&cfg, v);
                self.identities.reload(&cfg);
                tracing::info!(version = v, "reloaded repository and user table");
            }
            Err(e) => {
                tracing::error!(error = %e, "configuration reload failed; keeping previous table")
            }
        }
    }

    /// The repository for a client-supplied name.
    pub async fn repo(&self, name: &str) -> Result<Option<RepoConfig>, StorageError> {
        match &self.catalog {
            Some(c) => c.lookup(name).await,
            None => Ok(self.inner.read().unwrap().repos.get(name).cloned()),
        }
    }

    /// Repositories from the configuration file.
    pub fn configured_repos(&self) -> Vec<RepoConfig> {
        self.inner.read().unwrap().repos.values().cloned().collect()
    }

    /// Checks HTTP Basic credentials.
    pub async fn authenticate(&self, authorization: Option<&str>) -> Result<Identity, AuthError> {
        let Some(h) = authorization else {
            return Ok(Identity::Anonymous);
        };
        let (user, password) = parse_basic(h).ok_or(AuthError::BadCredentials)?;
        self.identities
            .authenticate(&user, &password)
            .await?
            .ok_or(AuthError::BadCredentials)
    }

    /// Checks a user name and password from the sign-in form.
    pub async fn sign_in(&self, user: &str, password: &str) -> Result<Identity, AuthError> {
        self.identities
            .authenticate(user, password)
            .await?
            .ok_or(AuthError::BadCredentials)
    }

    /// The user with this name and its current access, for web sessions.
    pub async fn lookup(&self, name: &str) -> Result<Option<NamedUser>, AuthError> {
        self.reload_if_changed();
        self.identities.lookup(name).await
    }

    /// Whether a push by `who` may create the missing repository `name`:
    /// only with the storage catalog, `catalog.create_on_push`, and at least
    /// that access (and write) to the name. Anonymous users never create.
    pub fn may_create_on_push(&self, who: &Identity, name: &str) -> bool {
        let Some(min) = self.inner.read().unwrap().create_on_push else {
            return false;
        };
        if self.catalog.is_none() || *who == Identity::Anonymous {
            return false;
        }
        let probe = RepoConfig::new(name, worktree_core::model::RepoId(uuid::Uuid::nil()));
        self.access(who, &probe).0 >= min.max(Access::Write)
    }

    /// The access level and the rule version it was computed with.
    pub fn access(&self, who: &Identity, repo: &RepoConfig) -> (Access, u64) {
        let base = who.access(&repo.name);
        let level = if repo.anonymous_read {
            base.max(Access::Read)
        } else {
            base
        };
        (level, self.inner.read().unwrap().version)
    }
}

fn parse_basic(h: &str) -> Option<(String, String)> {
    let encoded = h.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, password) = decoded.split_once(':')?;
    Some((user.to_string(), password.to_string()))
}

impl Inner {
    fn from(cfg: &Config, version: u64) -> Self {
        Self {
            version,
            repos: cfg
                .repos
                .iter()
                .map(|r| (r.name.clone(), r.clone()))
                .collect(),
            create_on_push: cfg.catalog.create_on_push,
        }
    }
}
