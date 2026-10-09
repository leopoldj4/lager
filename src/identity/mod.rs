//! Who a client is and what it may access
//! (docs/decisions/0008-users-in-postgres.md).
//!
//! An [`IdentityProvider`] checks a user name and password and answers with
//! the user's access levels; a provider that manages users also lists and
//! changes them for the admin API. The server uses one provider, built from
//! the configuration by [`from_config`]:
//!
//! - `[identity]`: an external service decides alone ([`service`]).
//! - Otherwise users in the configuration file ([`file`]), then, with
//!   `[database]`, users in PostgreSQL ([`postgres`]). File users take
//!   precedence, so the file always keeps a way in.

pub mod file;
pub mod postgres;
pub mod service;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::{Access, Config, validate_repo_name};
use crate::http::password::PasswordHash;

pub const MIN_PASSWORD_CHARS: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    Anonymous,
    /// A signed-in user with the access levels its provider granted.
    User {
        name: String,
        access: BTreeMap<String, Access>,
    },
}

impl Identity {
    pub fn name(&self) -> &str {
        match self {
            Identity::Anonymous => "anonymous",
            Identity::User { name, .. } => name,
        }
    }

    /// The access level for a repository: its own entry, else `"*"`.
    pub fn access(&self, repo: &str) -> Access {
        match self {
            Identity::Anonymous => Access::None,
            Identity::User { access, .. } => access
                .get(repo)
                .or_else(|| access.get("*"))
                .copied()
                .unwrap_or(Access::None),
        }
    }

    /// Admin access to every repository: needed for user and repository
    /// management.
    pub fn is_global_admin(&self) -> bool {
        match self {
            Identity::Anonymous => false,
            Identity::User { access, .. } => access.get("*") == Some(&Access::Admin),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Wrong or malformed credentials.
    BadCredentials,
    /// The identity service or database did not answer.
    Unavailable(String),
}

#[derive(Debug, thiserror::Error)]
pub enum UserError {
    #[error("user {0} exists")]
    Exists(String),
    #[error("user {0} not found")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    /// The user cannot be changed through this server.
    #[error("{0}")]
    Unmanaged(String),
    #[error("user store unavailable: {0}")]
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UserSource {
    /// `[[users]]` in the configuration file; read-only through the API.
    Config,
    Database,
}

/// A user as the admin API shows it. Never contains a password hash.
#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub name: String,
    /// Repository name (or "*") to access level.
    pub access: BTreeMap<String, Access>,
    pub source: UserSource,
    pub created_ms: Option<u64>,
    pub updated_ms: Option<u64>,
    pub updated_by: Option<String>,
}

pub struct NewUser {
    pub name: String,
    /// From [`PasswordHash`].
    pub password_hash: String,
    pub access: BTreeMap<String, Access>,
}

#[derive(Default)]
pub struct UserChange {
    pub password_hash: Option<String>,
    /// Replaces all access levels.
    pub access: Option<BTreeMap<String, Access>>,
}

#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Checks a user name and password. `Ok(None)`: no user with this name
    /// here, so another provider may know it.
    async fn authenticate(&self, user: &str, password: &str)
    -> Result<Option<Identity>, AuthError>;

    /// Whether [`create`](Self::create), [`update`](Self::update), and
    /// [`delete`](Self::delete) can work.
    fn manages_users(&self) -> bool {
        false
    }

    /// The users this provider knows, sorted by name.
    async fn list(&self) -> Result<Vec<User>, UserError> {
        Ok(vec![])
    }

    async fn create(&self, _new: NewUser, _by: &str) -> Result<User, UserError> {
        Err(unmanaged())
    }

    async fn update(&self, _name: &str, _change: UserChange, _by: &str) -> Result<User, UserError> {
        Err(unmanaged())
    }

    async fn delete(&self, _name: &str, _by: &str) -> Result<(), UserError> {
        Err(unmanaged())
    }

    /// Takes the users of a changed configuration file.
    fn reload(&self, _cfg: &Config) {}
}

/// The answer when users cannot be changed through this server.
pub fn unmanaged() -> UserError {
    UserError::Unmanaged(
        "users come from the configuration file or the identity service; \
         configure [database] without [identity] to manage them here"
            .into(),
    )
}

/// The provider for this configuration. Connects to the database and brings
/// its schema up to date.
pub async fn from_config(cfg: &Config) -> anyhow::Result<Arc<dyn IdentityProvider>> {
    if let Some(i) = &cfg.identity {
        return Ok(Arc::new(service::IdentityService::new(i)?));
    }
    let store: Option<Box<dyn IdentityProvider>> = match &cfg.database {
        Some(db) => Some(Box::new(postgres::PgUsers::connect(db).await?)),
        None => None,
    };
    Ok(Arc::new(Layered {
        file: file::FileUsers::new(cfg),
        store,
    }))
}

/// Users in the configuration file first, then a managed store. A stored
/// user with the name of a file user cannot sign in, is not listed, and
/// cannot be created or changed.
struct Layered {
    file: file::FileUsers,
    store: Option<Box<dyn IdentityProvider>>,
}

impl Layered {
    fn changeable(&self, name: &str) -> Result<&dyn IdentityProvider, UserError> {
        if self.file.contains(name) {
            return Err(UserError::Unmanaged(format!(
                "user {name} is defined in the configuration file"
            )));
        }
        self.store.as_deref().ok_or_else(unmanaged)
    }
}

#[async_trait]
impl IdentityProvider for Layered {
    async fn authenticate(
        &self,
        user: &str,
        password: &str,
    ) -> Result<Option<Identity>, AuthError> {
        // A file user with a wrong password fails here.
        if let Some(i) = self.file.authenticate(user, password).await? {
            return Ok(Some(i));
        }
        match &self.store {
            Some(s) => s.authenticate(user, password).await,
            None => Ok(None),
        }
    }

    fn manages_users(&self) -> bool {
        self.store.as_ref().is_some_and(|s| s.manages_users())
    }

    async fn list(&self) -> Result<Vec<User>, UserError> {
        let mut out = self.file.list().await?;
        if let Some(s) = &self.store {
            out.extend(
                s.list()
                    .await?
                    .into_iter()
                    .filter(|u| !self.file.contains(&u.name)),
            );
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn create(&self, new: NewUser, by: &str) -> Result<User, UserError> {
        self.changeable(&new.name)?.create(new, by).await
    }

    async fn update(&self, name: &str, change: UserChange, by: &str) -> Result<User, UserError> {
        self.changeable(name)?.update(name, change, by).await
    }

    async fn delete(&self, name: &str, by: &str) -> Result<(), UserError> {
        self.changeable(name)?.delete(name, by).await
    }

    fn reload(&self, cfg: &Config) {
        self.file.reload(cfg);
    }
}

/// User names appear in HTTP Basic credentials and in log entries.
pub fn validate_user_name(name: &str) -> Result<(), UserError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['.', '-'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(UserError::Invalid(format!(
            "invalid user name {name:?}: use 1 to 64 letters, digits, '.', '_', or '-'"
        )))
    }
}

pub fn validate_password(password: &str) -> Result<(), UserError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(UserError::Invalid(format!(
            "passwords need at least {MIN_PASSWORD_CHARS} characters"
        )));
    }
    Ok(())
}

pub fn validate_access(access: &BTreeMap<String, Access>) -> Result<(), UserError> {
    for name in access.keys() {
        if name != "*" {
            validate_repo_name(name).map_err(|e| UserError::Invalid(e.to_string()))?;
        }
    }
    Ok(())
}

/// How long a verified salted password stays verified.
const VERIFIED_FOR: Duration = Duration::from_secs(60);

/// Recently verified salted hashes, by digest of the credentials and the
/// hash: a slow hash for each Git request would dominate small fetches. The
/// key includes the hash, so a changed password is checked again at once.
#[derive(Default)]
struct VerifiedHashes(Mutex<HashMap<[u8; 32], Instant>>);

impl VerifiedHashes {
    /// Checks a [`PasswordHash`] off the request threads.
    async fn verify(&self, user: &str, password: &str, hash: &str) -> bool {
        let key = credential_digest(user, &format!("{password}\0{hash}"));
        if self
            .0
            .lock()
            .unwrap()
            .get(&key)
            .is_some_and(|t| t.elapsed() < VERIFIED_FOR)
        {
            return true;
        }
        let (password, hash) = (password.to_string(), hash.to_string());
        let ok = tokio::task::spawn_blocking(move || {
            PasswordHash::parse(&hash).is_ok_and(|h| h.verify(&password))
        })
        .await
        .unwrap_or(false);
        if ok {
            let mut verified = self.0.lock().unwrap();
            verified.retain(|_, t| t.elapsed() < VERIFIED_FOR);
            verified.insert(key, Instant::now());
        }
        ok
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

fn credential_digest(user: &str, password: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(user.as_bytes());
    h.update([0]);
    h.update(password.as_bytes());
    h.finalize().into()
}
