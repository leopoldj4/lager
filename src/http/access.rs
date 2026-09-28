//! Identity checks, repository names, and permissions.
//!
//! Identities come from the configuration file (development passwords on the
//! loopback address, salted hashes elsewhere) or from an external identity
//! service. Users can also be kept in storage (`crate::users`); users in the
//! file take precedence. Repository names come from the configuration file or
//! from the repository catalog in storage. The directory reloads the file when it
//! changes; each reload increases the permission-rule version that log
//! entries record.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use sha2::{Digest, Sha256};

use super::password::{PasswordHash, constant_time_eq};
use crate::catalog::Catalog;
use crate::config::{Access, Config, IdentityConfig, RepoConfig, UserConfig};
use crate::storage::StorageError;
use crate::users::UserStore;

pub struct Directory {
    inner: RwLock<Inner>,
    path: Option<PathBuf>,
    mtime: Mutex<Option<SystemTime>>,
    identity: Option<IdentityService>,
    catalog: Option<Arc<Catalog>>,
    users: Option<Arc<UserStore>>,
    /// Recently verified salted hashes, by digest of the credentials: a
    /// slow hash for each Git request would dominate small fetches.
    verified: Mutex<HashMap<[u8; 32], Instant>>,
}

/// How long a verified salted password stays verified.
const VERIFIED_FOR: Duration = Duration::from_secs(60);

struct Inner {
    version: u64,
    repos: HashMap<String, RepoConfig>,
    users: HashMap<String, UserConfig>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Wrong or malformed credentials.
    BadCredentials,
    /// The identity service did not answer.
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    Anonymous,
    /// A user from the configuration file.
    User(String),
    /// A user confirmed by the identity service, with the access it granted.
    External {
        name: String,
        access: BTreeMap<String, Access>,
    },
    /// A user from storage, with the access its record granted.
    Stored {
        name: String,
        access: BTreeMap<String, Access>,
    },
}

impl Identity {
    pub fn name(&self) -> &str {
        match self {
            Identity::Anonymous => "anonymous",
            Identity::User(u)
            | Identity::External { name: u, .. }
            | Identity::Stored { name: u, .. } => u,
        }
    }
}

impl Directory {
    pub fn new(cfg: &Config, path: Option<PathBuf>) -> anyhow::Result<Self> {
        let mtime = path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        let identity = cfg
            .identity
            .as_ref()
            .map(IdentityService::new)
            .transpose()?;
        Ok(Self {
            inner: RwLock::new(Inner::from(cfg, 1)),
            path,
            mtime: Mutex::new(mtime),
            identity,
            catalog: None,
            users: None,
            verified: Mutex::new(HashMap::new()),
        })
    }

    /// Takes repository names from the catalog instead of the file.
    pub fn with_catalog(mut self, catalog: Arc<Catalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    pub fn catalog(&self) -> Option<&Arc<Catalog>> {
        self.catalog.as_ref()
    }

    /// Also accepts users from storage.
    pub fn with_users(mut self, users: Arc<UserStore>) -> Self {
        self.users = Some(users);
        self
    }

    /// The user store, if users can be managed here: not when an identity
    /// service decides who the users are.
    pub fn user_store(&self) -> Option<&Arc<UserStore>> {
        self.users.as_ref().filter(|_| self.identity.is_none())
    }

    /// Users from the configuration file.
    pub fn configured_users(&self) -> Vec<UserConfig> {
        let mut users: Vec<_> = self.inner.read().unwrap().users.values().cloned().collect();
        users.sort_by(|a, b| a.name.cmp(&b.name));
        users
    }

    pub fn is_configured_user(&self, name: &str) -> bool {
        self.inner.read().unwrap().users.contains_key(name)
    }

    /// Admin access to every repository: needed for user management.
    pub fn is_global_admin(&self, who: &Identity) -> bool {
        let map = match who {
            Identity::Anonymous => return false,
            Identity::User(name) => {
                let inner = self.inner.read().unwrap();
                return inner
                    .users
                    .get(name)
                    .is_some_and(|u| u.access.get("*") == Some(&Access::Admin));
            }
            Identity::External { access, .. } | Identity::Stored { access, .. } => access,
        };
        map.get("*") == Some(&Access::Admin)
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
                self.verified.lock().unwrap().clear();
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
        if let Some(service) = &self.identity {
            return service.check(&user, &password).await;
        }
        let configured = self.inner.read().unwrap().users.get(&user).cloned();
        let Some(u) = configured else {
            return self.authenticate_stored(user, &password).await;
        };
        let ok = match (&u.password_hash, &u.password_sha256, &u.password) {
            (Some(h), _, _) => self.verify_hash(&user, &password, h),
            (None, Some(h), _) => hex::decode(h)
                .map(|h| constant_time_eq(&Sha256::digest(password.as_bytes()), &h))
                .unwrap_or(false),
            (None, None, Some(p)) => constant_time_eq(
                &Sha256::digest(password.as_bytes()),
                &Sha256::digest(p.as_bytes()),
            ),
            (None, None, None) => false,
        };
        if ok {
            Ok(Identity::User(user))
        } else {
            Err(AuthError::BadCredentials)
        }
    }

    async fn authenticate_stored(
        &self,
        user: String,
        password: &str,
    ) -> Result<Identity, AuthError> {
        let Some(store) = &self.users else {
            return Err(AuthError::BadCredentials);
        };
        let record = store
            .lookup(&user)
            .await
            .map_err(|e| AuthError::Unavailable(e.to_string()))?
            .ok_or(AuthError::BadCredentials)?;
        if self.verify_hash(&user, password, &record.password_hash) {
            Ok(Identity::Stored {
                name: user,
                access: record.access,
            })
        } else {
            Err(AuthError::BadCredentials)
        }
    }

    /// Checks a salted hash, reusing recent results. The key includes the
    /// hash, so a changed password is checked again.
    fn verify_hash(&self, user: &str, password: &str, hash: &str) -> bool {
        let key = credential_digest(user, &format!("{password}\0{hash}"));
        let mut verified = self.verified.lock().unwrap();
        if verified
            .get(&key)
            .is_some_and(|t| t.elapsed() < VERIFIED_FOR)
        {
            true
        } else if PasswordHash::parse(hash).is_ok_and(|h| h.verify(password)) {
            verified.retain(|_, t| t.elapsed() < VERIFIED_FOR);
            verified.insert(key, Instant::now());
            true
        } else {
            false
        }
    }

    /// The access level and the rule version it was computed with.
    pub fn access(&self, who: &Identity, repo: &RepoConfig) -> (Access, u64) {
        let inner = self.inner.read().unwrap();
        let granted = |map: Option<&BTreeMap<String, Access>>| {
            let specific = map.and_then(|m| m.get(&repo.name)).copied();
            let wildcard = map.and_then(|m| m.get("*")).copied();
            specific.or(wildcard).unwrap_or(Access::None)
        };
        let base = match who {
            Identity::Anonymous => Access::None,
            Identity::User(name) => granted(inner.users.get(name).map(|u| &u.access)),
            Identity::External { access, .. } | Identity::Stored { access, .. } => {
                granted(Some(access))
            }
        };
        let level = if repo.anonymous_read {
            base.max(Access::Read)
        } else {
            base
        };
        (level, inner.version)
    }
}

fn credential_digest(user: &str, password: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(user.as_bytes());
    h.update([0]);
    h.update(password.as_bytes());
    h.finalize().into()
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
            users: cfg
                .users
                .iter()
                .map(|u| (u.name.clone(), u.clone()))
                .collect(),
        }
    }
}

/// Checks credentials with an external identity service and keeps positive
/// answers for a short time. Credentials are cached only as a SHA-256 digest.
struct IdentityService {
    cfg: IdentityConfig,
    client: reqwest::Client,
    cache: Mutex<HashMap<[u8; 32], (Instant, Identity)>>,
}

#[derive(serde::Deserialize)]
struct IdentityAnswer {
    user: String,
    #[serde(default)]
    access: BTreeMap<String, Access>,
}

impl IdentityService {
    fn new(cfg: &IdentityConfig) -> anyhow::Result<Self> {
        let mut b = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(ca) = &cfg.ca_certificate {
            b = b.add_root_certificate(reqwest::Certificate::from_pem(&std::fs::read(ca)?)?);
        }
        Ok(Self {
            cfg: cfg.clone(),
            client: b.build()?,
            cache: Mutex::new(HashMap::new()),
        })
    }

    async fn check(&self, user: &str, password: &str) -> Result<Identity, AuthError> {
        let key = credential_digest(user, password);
        let ttl = Duration::from_secs(self.cfg.cache_secs);
        if let Some((at, id)) = self.cache.lock().unwrap().get(&key)
            && at.elapsed() < ttl
        {
            return Ok(id.clone());
        }
        let mut req = self
            .client
            .post(&self.cfg.url)
            .header("content-type", "application/json")
            .body(
                serde_json::to_vec(&serde_json::json!({"username": user, "password": password}))
                    .expect("serializes"),
            );
        if let Some(t) = &self.cfg.token {
            req = req.bearer_auth(t);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AuthError::Unavailable(e.without_url().to_string()))?;
        match resp.status().as_u16() {
            200 => {}
            401 | 403 => return Err(AuthError::BadCredentials),
            s => {
                return Err(AuthError::Unavailable(format!(
                    "identity service status {s}"
                )));
            }
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| AuthError::Unavailable(e.without_url().to_string()))?;
        let answer: IdentityAnswer = serde_json::from_slice(&body)
            .map_err(|e| AuthError::Unavailable(format!("identity service answer: {e}")))?;
        let id = Identity::External {
            name: answer.user,
            access: answer.access,
        };
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|_, (at, _)| at.elapsed() < ttl);
        cache.insert(key, (Instant::now(), id.clone()));
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> Directory {
        let hash = PasswordHash::new("hashed-pw", 10_000).to_string();
        let cfg: Config = toml::from_str(&format!(
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
            access = {{ a = "force" }}
            [[users]]
            name = "bob"
            password_sha256 = "8d969eef6ecad3c29a3a629280e686cf0c3f5d5a86aff3ca12020c923adc6c92"
            access = {{ "*" = "read" }}
            [[users]]
            name = "erin"
            password_hash = "{hash}"
            access = {{ a = "write" }}
            "#
        ))
        .unwrap();
        cfg.validate().unwrap();
        Directory::new(&cfg, None).unwrap()
    }

    fn basic(u: &str, p: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
        )
    }

    #[tokio::test]
    async fn credentials_are_checked_against_plain_and_hashed_passwords() {
        let d = dir();
        let auth = async |u: &str, p: &str| d.authenticate(Some(&basic(u, p))).await;
        assert_eq!(
            auth("alice", "pw").await,
            Ok(Identity::User("alice".into()))
        );
        assert_eq!(
            auth("bob", "123456").await,
            Ok(Identity::User("bob".into()))
        );
        assert_eq!(
            auth("erin", "hashed-pw").await,
            Ok(Identity::User("erin".into()))
        );
        // The second check uses the cache of verified hashes.
        assert_eq!(
            auth("erin", "hashed-pw").await,
            Ok(Identity::User("erin".into()))
        );
        for (u, p) in [("alice", "wrong"), ("erin", "hashed-p"), ("mallory", "pw")] {
            assert_eq!(auth(u, p).await, Err(AuthError::BadCredentials), "{u}");
        }
        assert!(d.authenticate(Some("Bearer x")).await.is_err());
        assert_eq!(d.authenticate(None).await, Ok(Identity::Anonymous));
    }

    #[tokio::test]
    async fn access_uses_specific_rules_then_wildcards_then_anonymous_read() {
        let d = dir();
        let repo = |n: &str| d.inner.read().unwrap().repos[n].clone();
        let user = |n: &str| Identity::User(n.into());
        assert_eq!(d.access(&user("alice"), &repo("a")).0, Access::Force);
        assert_eq!(d.access(&user("alice"), &repo("pub")).0, Access::Read);
        assert_eq!(d.access(&user("bob"), &repo("a")).0, Access::Read);
        assert_eq!(d.access(&Identity::Anonymous, &repo("a")).0, Access::None);
        assert_eq!(d.access(&Identity::Anonymous, &repo("pub")).0, Access::Read);
        let ext = Identity::External {
            name: "x".into(),
            access: [("*".to_string(), Access::Write)].into(),
        };
        assert_eq!(d.access(&ext, &repo("a")).0, Access::Write);
    }

    #[test]
    fn development_credentials_are_rejected_off_the_loopback_address() {
        let text = |user: &str| {
            format!(
                r#"
                [server]
                listen = "0.0.0.0:1"
                cache_dir = "/tmp/x"
                tls_terminated_upstream = true
                [storage]
                kind = "memory"
                [[users]]
                name = "u"
                {user}
                "#
            )
        };
        for dev in ["password = \"pw\"", "password_sha256 = \"00\""] {
            let cfg: Config = toml::from_str(&text(dev)).unwrap();
            let err = cfg.validate().unwrap_err().to_string();
            assert!(err.contains("password_hash"), "{err}");
        }
        let hash = format!("password_hash = \"{}\"", PasswordHash::new("pw", 10_000));
        let cfg: Config = toml::from_str(&text(&hash)).unwrap();
        cfg.validate().unwrap();
    }
}
