//! Identity checks and permissions, from the configuration file.
//!
//! The directory reloads when the file changes. Each reload increases the
//! permission-rule version that log entries record.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::SystemTime;

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::config::{Access, Config, RepoConfig, UserConfig};

pub struct Directory {
    inner: RwLock<Inner>,
    path: Option<PathBuf>,
    mtime: Mutex<Option<SystemTime>>,
}

struct Inner {
    version: u64,
    repos: HashMap<String, RepoConfig>,
    users: HashMap<String, UserConfig>,
}

/// Wrong or malformed credentials.
#[derive(Debug, PartialEq, Eq)]
pub struct BadCredentials;

#[derive(Debug, PartialEq, Eq)]
pub enum Identity {
    Anonymous,
    User(String),
}

impl Identity {
    pub fn name(&self) -> &str {
        match self {
            Identity::Anonymous => "anonymous",
            Identity::User(u) => u,
        }
    }
}

impl Directory {
    pub fn new(cfg: &Config, path: Option<PathBuf>) -> Self {
        let mtime = path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        Self {
            inner: RwLock::new(Inner::from(cfg, 1)),
            path,
            mtime: Mutex::new(mtime),
        }
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
                tracing::info!(version = v, "reloaded repository and user table");
            }
            Err(e) => {
                tracing::error!(error = %e, "configuration reload failed; keeping previous table")
            }
        }
    }

    pub fn repo(&self, name: &str) -> Option<RepoConfig> {
        self.inner.read().unwrap().repos.get(name).cloned()
    }

    pub fn repos(&self) -> Vec<RepoConfig> {
        self.inner.read().unwrap().repos.values().cloned().collect()
    }

    /// Checks HTTP Basic credentials. `Err` for wrong credentials.
    pub fn authenticate(&self, authorization: Option<&str>) -> Result<Identity, BadCredentials> {
        let Some(h) = authorization else {
            return Ok(Identity::Anonymous);
        };
        let encoded = h.strip_prefix("Basic ").ok_or(BadCredentials)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| BadCredentials)?;
        let decoded = String::from_utf8(decoded).map_err(|_| BadCredentials)?;
        let (user, password) = decoded.split_once(':').ok_or(BadCredentials)?;
        let inner = self.inner.read().unwrap();
        let u = inner.users.get(user).ok_or(BadCredentials)?;
        let given = Sha256::digest(password.as_bytes());
        let expected = match (&u.password_sha256, &u.password) {
            (Some(h), _) => hex::decode(h).map_err(|_| BadCredentials)?,
            (None, Some(p)) => Sha256::digest(p.as_bytes()).to_vec(),
            (None, None) => return Err(BadCredentials),
        };
        if constant_time_eq(&given, &expected) {
            Ok(Identity::User(user.to_string()))
        } else {
            Err(BadCredentials)
        }
    }

    /// The access level and the rule version it was computed with.
    pub fn access(&self, who: &Identity, repo: &str) -> (Access, u64) {
        let inner = self.inner.read().unwrap();
        let Some(r) = inner.repos.get(repo) else {
            return (Access::None, inner.version);
        };
        let level = match who {
            Identity::Anonymous => {
                if r.anonymous_read {
                    Access::Read
                } else {
                    Access::None
                }
            }
            Identity::User(name) => {
                let u = inner.users.get(name);
                let specific = u.and_then(|u| u.access.get(repo)).copied();
                let wildcard = u.and_then(|u| u.access.get("*")).copied();
                let base = specific.or(wildcard).unwrap_or(Access::None);
                if r.anonymous_read {
                    base.max(Access::Read)
                } else {
                    base
                }
            }
        };
        (level, inner.version)
    }
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
            users: cfg
                .users
                .iter()
                .map(|u| (u.name.clone(), u.clone()))
                .collect(),
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> Directory {
        let cfg: Config = toml::from_str(
            r#"
            [server]
            listen = "127.0.0.1:1"
            cache_dir = "/tmp/x"
            [storage]
            kind = "memory"
            [[repos]]
            name = "a"
            id = "00000000-0000-0000-0000-000000000001"
            [[repos]]
            name = "pub"
            id = "00000000-0000-0000-0000-000000000002"
            anonymous_read = true
            [[users]]
            name = "alice"
            password = "pw"
            access = { a = "force" }
            [[users]]
            name = "bob"
            password_sha256 = "8d969eef6ecad3c29a3a629280e686cf0c3f5d5a86aff3ca12020c923adc6c92"
            access = { "*" = "read" }
            "#,
        )
        .unwrap();
        Directory::new(&cfg, None)
    }

    fn basic(u: &str, p: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
        )
    }

    #[test]
    fn credentials_are_checked_against_plain_and_hashed_passwords() {
        let d = dir();
        assert_eq!(
            d.authenticate(Some(&basic("alice", "pw"))),
            Ok(Identity::User("alice".into()))
        );
        assert_eq!(
            d.authenticate(Some(&basic("bob", "123456"))),
            Ok(Identity::User("bob".into()))
        );
        assert!(d.authenticate(Some(&basic("alice", "wrong"))).is_err());
        assert!(d.authenticate(Some(&basic("mallory", "pw"))).is_err());
        assert!(d.authenticate(Some("Bearer x")).is_err());
        assert_eq!(d.authenticate(None), Ok(Identity::Anonymous));
    }

    #[test]
    fn access_uses_specific_rules_then_wildcards_then_anonymous_read() {
        let d = dir();
        assert_eq!(
            d.access(&Identity::User("alice".into()), "a").0,
            Access::Force
        );
        assert_eq!(
            d.access(&Identity::User("alice".into()), "pub").0,
            Access::Read
        );
        assert_eq!(d.access(&Identity::User("bob".into()), "a").0, Access::Read);
        assert_eq!(d.access(&Identity::Anonymous, "a").0, Access::None);
        assert_eq!(d.access(&Identity::Anonymous, "pub").0, Access::Read);
        assert_eq!(d.access(&Identity::Anonymous, "missing").0, Access::None);
    }
}
