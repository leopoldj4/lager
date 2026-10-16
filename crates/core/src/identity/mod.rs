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
//!
//! Git over SSH signs in with a user's public key
//! (docs/decisions/0022-git-over-ssh.md): the key has its user's access.

pub mod passkeys;
pub mod postgres;

pub use postgres::PgUsers;

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
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
        /// The levels of the token used to sign in, if it has any: the user
        /// then has at most these.
        token: Option<BTreeMap<String, Access>>,
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

    /// The access level for a repository: `force` for the user's own
    /// repositories (`<user>/...`), else its own entry, else `"*"`; never
    /// more than the token allows.
    pub fn access(&self, repo: &str) -> Access {
        match self {
            Identity::Anonymous => Access::None,
            Identity::User {
                name,
                access,
                token,
            } => {
                let own = repo.split_once('/').is_some_and(|(owner, _)| owner == name);
                let user = if own {
                    Access::Force
                } else {
                    level(access, repo)
                };
                match token {
                    Some(token) => user.min(level(token, repo)),
                    None => user,
                }
            }
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
    #[error("SSH key {0} not found")]
    SshKeyNotFound(SshKeyId),
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

pub type SshKeyId = i64;

/// A user's SSH public key for Git over SSH.
#[derive(Clone, Debug)]
pub struct SshKey {
    pub id: SshKeyId,
    pub name: String,
    /// `SHA256:` and the unpadded base64 digest of the key, as
    /// `ssh-keygen -l` shows it.
    pub fingerprint: String,
    /// The key in `authorized_keys` form, without the comment.
    pub public_key: String,
    pub created_ms: u64,
    /// Updated at most once a minute.
    pub last_used_ms: Option<u64>,
}

/// A parsed public key line, such as a line of `~/.ssh/id_ed25519.pub`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKeyLine {
    pub algorithm: String,
    /// The key in SSH wire encoding.
    pub blob: Vec<u8>,
    /// The comment after the key, often `user@host`.
    pub comment: Option<String>,
}

impl PublicKeyLine {
    /// The key in `authorized_keys` form, without the comment.
    pub fn openssh(&self) -> String {
        format!(
            "{} {}",
            self.algorithm,
            base64::engine::general_purpose::STANDARD.encode(&self.blob)
        )
    }

    pub fn fingerprint(&self) -> String {
        ssh_fingerprint(&self.blob)
    }
}

/// Key types that the SSH server verifies. DSA and certificates are not
/// accepted.
const SSH_KEY_ALGORITHMS: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "ssh-rsa",
];

/// Parses `<type> <base64> [comment]`. The type inside the encoded key must
/// match the one in front of it.
pub fn parse_public_key(line: &str) -> Result<PublicKeyLine, UserError> {
    let invalid = |m: &str| UserError::Invalid(format!("invalid SSH public key: {m}"));
    let mut parts = line.trim().splitn(3, char::is_whitespace);
    let algorithm = parts.next().unwrap_or_default();
    if !SSH_KEY_ALGORITHMS.contains(&algorithm) {
        return Err(invalid(&format!(
            "type {algorithm:?} is not one of {}",
            SSH_KEY_ALGORITHMS.join(", ")
        )));
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(parts.next().unwrap_or_default())
        .map_err(|_| invalid("the key is not base64"))?;
    // The blob starts with the type as an SSH string.
    let inner = blob
        .get(..4)
        .map(|n| u32::from_be_bytes(n.try_into().expect("4 bytes")) as usize)
        .and_then(|n| blob.get(4..4 + n));
    if inner != Some(algorithm.as_bytes()) || blob.len() > 16 * 1024 {
        return Err(invalid("the encoded key does not match its type"));
    }
    let comment = parts
        .next()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    Ok(PublicKeyLine {
        algorithm: algorithm.to_string(),
        blob,
        comment,
    })
}

/// The SHA-256 fingerprint of a key in SSH wire encoding.
pub fn ssh_fingerprint(blob: &[u8]) -> String {
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(blob))
    )
}

/// SSH key names are labels for people, such as "laptop".
pub fn validate_ssh_key_name(name: &str) -> Result<(), UserError> {
    let chars = name.chars().count();
    if !(1..=100).contains(&chars) || name.chars().any(char::is_control) {
        return Err(UserError::Invalid(
            "SSH key names need 1 to 100 characters and no control characters".into(),
        ));
    }
    Ok(())
}

fn level(access: &BTreeMap<String, Access>, repo: &str) -> Access {
    access
        .get(repo)
        .or_else(|| access.get("*"))
        .copied()
        .unwrap_or(Access::None)
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
