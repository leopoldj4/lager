//! Configuration file (TOML). It holds only what a server needs to start:
//! its address, the storage service, the database, the change bus, the
//! web session secret, the GitHub OAuth app, and the SSH host key
//! (docs/decisions/0015-minimal-configuration.md).
//! Repositories live in the catalog in storage, users in the database, and
//! limits are fixed in [`crate::limits`].

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub bus: Option<BusConfig>,
    #[serde(default)]
    pub web: Option<WebConfig>,
    #[serde(default)]
    pub github: Option<GithubConfig>,
    pub ssh: Option<SshConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Plain HTTP: a load balancer terminates HTTPS in front of the server.
    pub listen: SocketAddr,
    /// Local caches; can be removed at any time.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
}

/// An S3-compatible service with conditional writes (`If-Match`,
/// `If-None-Match`).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// For example `https://<account>.r2.cloudflarestorage.com`. Without it,
    /// AWS S3. An `http://` endpoint is permitted for local development.
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default = "default_region")]
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Key prefix inside the bucket.
    #[serde(default)]
    pub prefix: String,
}

/// PostgreSQL for users and their access tokens
/// (docs/decisions/0008-users-in-postgres.md).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// For example `postgres://lager:...@db.example.com/lager?sslmode=verify-full`.
    pub url: String,
}

/// NATS for change events between servers
/// (docs/decisions/0012-nats-change-bus.md). Without it, events stay on
/// this server.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BusConfig {
    /// For example `nats://nats.internal:4222` or `tls://user:...@nats.example.com:4222`.
    pub url: String,
}

/// The web interface and its API (docs/decisions/0014-web-interface.md).
/// Without it, the server answers only Git requests.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Signs session cookies: at least 32 characters, the same on every
    /// server. A new secret signs every user out.
    pub session_secret: String,
    /// Browser-facing origin of this deployment, e.g. https://git.example.com.
    /// Used to verify passkey requests; omit to disable passkeys.
    #[serde(default)]
    pub public_origin: Option<String>,
    /// Lets anyone create an account with a passkey and no password
    /// (docs/decisions/0023-passkey-sign-up.md). Needs `public_origin`.
    #[serde(default)]
    pub sign_up: bool,
}

/// A GitHub OAuth app for read-only mirrors of GitHub repositories
/// (docs/decisions/0020-github-mirrors.md). Its callback URL is
/// `<origin>/-/api/github/callback`. Without it, nobody can connect GitHub.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    pub client_id: String,
    pub client_secret: String,
}

/// Git over SSH (docs/decisions/0022-git-over-ssh.md). Without it, Git
/// clients use only HTTP.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshConfig {
    pub listen: SocketAddr,
    /// An OpenSSH private key without a passphrase, the same file on every
    /// server: clients check that the host key never changes.
    pub host_key: PathBuf,
    /// The SSH address that clients clone from, shown in the web interface,
    /// such as `ssh://git@git.example.com` or `ssh://git@git.example.com:2222`.
    #[serde(default)]
    pub public_url: Option<String>,
}

fn default_cache_dir() -> PathBuf {
    PathBuf::from("/var/cache/lager")
}
fn default_region() -> String {
    "us-east-1".into()
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.database.url.is_empty(), "database.url is empty");
        if let Some(b) = &self.bus {
            anyhow::ensure!(!b.url.is_empty(), "bus.url is empty");
        }
        if let Some(w) = &self.web {
            if let Some(origin) = &w.public_origin {
                crate::identity::passkeys::webauthn(origin)?;
            }
            anyhow::ensure!(
                !w.sign_up || w.public_origin.is_some(),
                "web.sign_up needs web.public_origin"
            );
            anyhow::ensure!(
                w.session_secret.chars().count() >= 32,
                "web.session_secret needs at least 32 characters"
            );
        }
        if let Some(g) = &self.github {
            anyhow::ensure!(self.web.is_some(), "[github] needs [web]");
            anyhow::ensure!(
                !g.client_id.is_empty() && !g.client_secret.is_empty(),
                "github.client_id and github.client_secret are required"
            );
        }
        if let Some(s) = &self.ssh
            && let Some(url) = &s.public_url
        {
            anyhow::ensure!(
                url.starts_with("ssh://") && !url.ends_with('/'),
                "ssh.public_url must start with ssh:// and not end with /"
            );
        }
        Ok(())
    }

    /// The hook socket. Unix socket paths are short (about 100 bytes), so a
    /// path in a deep cache directory moves to a private directory under the
    /// system temporary directory, named after the cache directory.
    pub fn socket_path(&self) -> PathBuf {
        let default = self.server.cache_dir.join("hook.sock");
        if default.as_os_str().len() < MAX_SOCKET_PATH {
            return default;
        }
        use sha2::Digest;
        let digest = sha2::Sha256::digest(self.server.cache_dir.as_os_str().as_encoded_bytes());
        std::env::temp_dir()
            .join(format!("lager-{}", &hex::encode(digest)[..16]))
            .join("hook.sock")
    }
}

/// Longest Unix socket path that works on Linux and macOS.
pub const MAX_SOCKET_PATH: usize = 100;
