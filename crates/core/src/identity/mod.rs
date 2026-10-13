//! Who a client is and what it may access
//! (docs/decisions/0008-users-in-postgres.md).
//!
//! Users and their access levels live in PostgreSQL ([`postgres`]); they
//! are managed with `worktree user` and `worktree token`. [`PgUsers`] checks
//! a user name and password and answers with the user's access levels.
//!
//! Users can also sign in with personal access tokens
//! (docs/decisions/0009-personal-access-tokens.md): the token takes the
//! place of the password, and can limit the user's access further.

pub mod passkeys;
pub mod postgres;

pub use postgres::PgUsers;

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::password::PasswordHash;
use crate::repo::{Access, validate_repo_name};

pub const MIN_PASSWORD_CHARS: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    Anonymous,
    /// A signed-in user with its access levels.
    User {
        name: String,
        access: BTreeMap<String, Access>,
    },
}

pub type TokenId = i64;

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
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Wrong or malformed credentials.
    BadCredentials,
    /// The database did not answer.
    Unavailable(String),
}

#[derive(Debug, thiserror::Error)]
pub enum UserError {
    #[error("user {0} exists")]
    Exists(String),
    #[error("user {0} not found")]
    NotFound(String),
    #[error("token {0} not found")]
    TokenNotFound(TokenId),
    #[error("{0}")]
    Invalid(String),
    #[error("user store unavailable: {0}")]
    Unavailable(String),
}

/// A user without its password hash.
#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    /// Repository name (or "*") to access level.
    pub access: BTreeMap<String, Access>,
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

/// Prefix of every personal access token, so that scanners and people can
/// recognize one.
pub const TOKEN_PREFIX: &str = "rgpat_";

/// A personal access token without its secret. The token itself is shown
/// once, when it is created, and never stored.
#[derive(Clone, Debug)]
pub struct Token {
    pub id: TokenId,
    pub name: String,
    /// `None`: the user's access. Otherwise at most these levels, and never
    /// more than the user has.
    pub access: Option<BTreeMap<String, Access>>,
    pub created_ms: u64,
    pub expires_ms: Option<u64>,
    /// Updated at most once a minute.
    pub last_used_ms: Option<u64>,
}

pub struct NewToken {
    pub name: String,
    pub access: Option<BTreeMap<String, Access>>,
    pub expires_days: Option<u32>,
}

/// The lower of two access levels for every repository: a token never
/// grants more than its user has. Repositories that neither map names fall
/// back to the lower `"*"` level, as [`Identity::access`] does.
pub fn restrict(
    user: &BTreeMap<String, Access>,
    token: &BTreeMap<String, Access>,
) -> BTreeMap<String, Access> {
    let level = |m: &BTreeMap<String, Access>, k: &str| {
        m.get(k)
            .or_else(|| m.get("*"))
            .copied()
            .unwrap_or(Access::None)
    };
    user.keys()
        .chain(token.keys())
        .chain(std::iter::once(&"*".to_string()))
        .map(|k| (k.clone(), level(user, k).min(level(token, k))))
        .collect()
}

/// A user found by name, for web sessions
/// (docs/decisions/0014-web-interface.md).
#[derive(Clone, Debug)]
pub struct NamedUser {
    pub identity: Identity,
    /// A digest of the stored password: it changes with the password, which
    /// ends the user's sessions.
    pub stamp: [u8; 32],
}

/// Connects to the database and brings its schema up to date.
pub async fn connect(cfg: &crate::config::Config) -> anyhow::Result<PgUsers> {
    PgUsers::connect(&cfg.database).await
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
    // Such a password would be taken for a token.
    if password.starts_with(TOKEN_PREFIX) {
        return Err(UserError::Invalid(format!(
            "passwords cannot start with {TOKEN_PREFIX}"
        )));
    }
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

/// Token names are labels for people, such as "CI for org/app".
pub fn validate_token(new: &NewToken) -> Result<(), UserError> {
    let chars = new.name.chars().count();
    if !(1..=100).contains(&chars) || new.name.chars().any(char::is_control) {
        return Err(UserError::Invalid(
            "token names need 1 to 100 characters and no control characters".into(),
        ));
    }
    if let Some(access) = &new.access {
        if access.is_empty() {
            return Err(UserError::Invalid(
                "a token's access needs at least one repository or \"*\"; \
                 leave it out for the user's access"
                    .into(),
            ));
        }
        validate_access(access)?;
    }
    if new.expires_days.is_some_and(|d| !(1..=3650).contains(&d)) {
        return Err(UserError::Invalid(
            "expires_days must be between 1 and 3650".into(),
        ));
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
}

/// The session stamp of a stored password.
fn password_stamp(stored: &str) -> [u8; 32] {
    credential_digest("worktree-session-stamp", stored)
}

fn credential_digest(user: &str, password: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(user.as_bytes());
    h.update([0]);
    h.update(password.as_bytes());
    h.finalize().into()
}
