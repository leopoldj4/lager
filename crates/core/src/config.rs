//! Configuration from environment variables. It holds only what a server
//! needs to start: its address, the storage service, the database, the
//! change bus, the web session secret, the GitHub OAuth app, and the SSH
//! host key (docs/decisions/0015-minimal-configuration.md,
//! docs/decisions/0024-configuration-in-the-environment.md).
//! Repositories live in the catalog in storage, users in the database, and
//! limits are fixed in [`crate::limits`]. An empty variable counts as unset.
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `LAGER_DOMAIN` | | Public host name: `https://<domain>` for passkeys and GitHub, `ssh://git@<domain>` for cloning; `localhost[:port]` for development |
//! | `LAGER_LISTEN` | `0.0.0.0:8080` | Plain HTTP |
//! | `LAGER_CACHE_DIR` | `/var/cache/lager` | |
//! | `LAGER_S3_ENDPOINT` | AWS S3 | |
//! | `LAGER_S3_REGION` | `us-east-1` | |
//! | `LAGER_S3_BUCKET` | required | |
//! | `LAGER_S3_ACCESS_KEY_ID` | required | |
//! | `LAGER_S3_SECRET_ACCESS_KEY` | required | |
//! | `LAGER_S3_PREFIX` | empty | |
//! | `LAGER_DATABASE_URL` | required | |
//! | `LAGER_NATS_URL` | | Without it, events stay on this server |
//! | `LAGER_SESSION_SECRET` | | Turns on the web interface |
//! | `LAGER_SIGN_UP` | `false` | Needs `LAGER_DOMAIN` |
//! | `LAGER_GITHUB_CLIENT_ID`, `LAGER_GITHUB_CLIENT_SECRET` | | Both or neither |
//! | `LAGER_SSH_HOST_KEY` | | Turns on Git over SSH |
//! | `LAGER_SSH_LISTEN` | `0.0.0.0:2222` | |

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub database: DatabaseConfig,
    pub bus: Option<BusConfig>,
    pub web: Option<WebConfig>,
    pub github: Option<GithubConfig>,
    pub ssh: Option<SshConfig>,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Plain HTTP: a load balancer terminates HTTPS in front of the server.
    pub listen: SocketAddr,
    /// Local caches; can be removed at any time.
    pub cache_dir: PathBuf,
}

/// An S3-compatible service with conditional writes (`If-Match`,
/// `If-None-Match`).
#[derive(Clone, Debug)]
pub struct StorageConfig {
    /// For example `https://<account>.r2.cloudflarestorage.com`. Without it,
    /// AWS S3. An `http://` endpoint is permitted for local development.
    pub endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Key prefix inside the bucket.
    pub prefix: String,
}

/// PostgreSQL for users and their access tokens
/// (docs/decisions/0008-users-in-postgres.md).
#[derive(Clone, Debug)]
pub struct DatabaseConfig {
    /// For example `postgres://lager:...@db.example.com/lager?sslmode=verify-full`.
    pub url: String,
}

/// NATS for change events between servers
/// (docs/decisions/0012-nats-change-bus.md). Without it, events stay on
/// this server.
#[derive(Clone, Debug)]
pub struct BusConfig {
    /// For example `nats://nats.internal:4222` or `tls://user:...@nats.example.com:4222`.
    pub url: String,
}

/// The web interface and its API (docs/decisions/0014-web-interface.md).
/// Without it, the server answers only Git requests.
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// Signs session cookies: at least 32 characters, the same on every
    /// server. A new secret signs every user out.
    pub session_secret: String,
    /// `https://<LAGER_DOMAIN>`, or `http://` for `localhost`. Used to
    /// verify passkey requests; without it, passkeys are off.
    pub public_origin: Option<String>,
    /// Lets anyone create an account with a passkey and no password
    /// (docs/decisions/0023-passkey-sign-up.md). Needs `public_origin`.
    pub sign_up: bool,
}

/// A GitHub OAuth app for read-only mirrors of GitHub repositories
/// (docs/decisions/0020-github-mirrors.md). Its callback URL is
/// `<origin>/-/api/github/callback`. Without it, nobody can connect GitHub.
#[derive(Clone, Debug)]
pub struct GithubConfig {
    pub client_id: String,
    pub client_secret: String,
}

/// Git over SSH (docs/decisions/0022-git-over-ssh.md). Without it, Git
/// clients use only HTTP.
#[derive(Clone, Debug)]
pub struct SshConfig {
    pub listen: SocketAddr,
    /// An OpenSSH private key without a passphrase, the same on every
    /// server: clients check that the host key never changes.
    pub host_key: String,
    /// The SSH address that clients clone from, shown in the web interface:
    /// `ssh://git@<LAGER_DOMAIN>`, or for local development the listen
    /// address on the loopback address.
    pub public_url: Option<String>,
}

impl Config {
    /// Reads the process environment.
    pub fn from_env() -> anyhow::Result<Config> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Config> {
        let get = |name: &str| var(name).filter(|v| !v.is_empty());
        let required = |name: &str| get(name).ok_or_else(|| anyhow::anyhow!("{name} is not set"));
        let addr = |name: &str, default: &str| -> anyhow::Result<SocketAddr> {
            let v = get(name).unwrap_or_else(|| default.into());
            v.parse()
                .map_err(|e| anyhow::anyhow!("{name}: {v:?} is not an address and port: {e}"))
        };

        let domain = get("LAGER_DOMAIN");
        // `localhost` and `localhost:<port>` for development: plain HTTP,
        // and SSH on its own port.
        let local = domain
            .as_deref()
            .is_some_and(|d| d == "localhost" || d.starts_with("localhost:"));
        if let Some(d) = &domain {
            anyhow::ensure!(
                (local || !d.contains(':'))
                    && !d.contains(['/', '@'])
                    && !d.contains(char::is_whitespace),
                "LAGER_DOMAIN must be a host name, such as git.example.com"
            );
        }
        let public_origin = domain
            .as_ref()
            .map(|d| format!("{}://{d}", if local { "http" } else { "https" }));

        let server = ServerConfig {
            listen: addr("LAGER_LISTEN", "0.0.0.0:8080")?,
            cache_dir: get("LAGER_CACHE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/cache/lager")),
        };
        let storage = StorageConfig {
            endpoint: get("LAGER_S3_ENDPOINT"),
            region: get("LAGER_S3_REGION").unwrap_or_else(|| "us-east-1".into()),
            bucket: required("LAGER_S3_BUCKET")?,
            access_key_id: required("LAGER_S3_ACCESS_KEY_ID")?,
            secret_access_key: required("LAGER_S3_SECRET_ACCESS_KEY")?,
            prefix: get("LAGER_S3_PREFIX").unwrap_or_default(),
        };
        let database = DatabaseConfig {
            url: required("LAGER_DATABASE_URL")?,
        };
        let bus = get("LAGER_NATS_URL").map(|url| BusConfig { url });

        let sign_up = match get("LAGER_SIGN_UP").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(v) => anyhow::bail!("LAGER_SIGN_UP must be true or false, not {v:?}"),
        };
        let web = get("LAGER_SESSION_SECRET").map(|session_secret| WebConfig {
            session_secret,
            public_origin: public_origin.clone(),
            sign_up,
        });
        if let Some(w) = &web {
            anyhow::ensure!(
                w.session_secret.chars().count() >= 32,
                "LAGER_SESSION_SECRET needs at least 32 characters"
            );
            if let Some(origin) = &w.public_origin {
                crate::identity::passkeys::webauthn(origin)?;
            }
        }
        anyhow::ensure!(
            !sign_up || domain.is_some(),
            "LAGER_SIGN_UP needs LAGER_DOMAIN"
        );
        anyhow::ensure!(
            !sign_up || web.is_some(),
            "LAGER_SIGN_UP needs LAGER_SESSION_SECRET"
        );

        let github = match (
            get("LAGER_GITHUB_CLIENT_ID"),
            get("LAGER_GITHUB_CLIENT_SECRET"),
        ) {
            (Some(client_id), Some(client_secret)) => Some(GithubConfig {
                client_id,
                client_secret,
            }),
            (None, None) => None,
            _ => anyhow::bail!("set both LAGER_GITHUB_CLIENT_ID and LAGER_GITHUB_CLIENT_SECRET"),
        };
        anyhow::ensure!(
            github.is_none() || web.is_some(),
            "LAGER_GITHUB_CLIENT_ID needs LAGER_SESSION_SECRET"
        );

        let ssh = match get("LAGER_SSH_HOST_KEY") {
            Some(key) => {
                let listen = addr("LAGER_SSH_LISTEN", "0.0.0.0:2222")?;
                let public_url = match &domain {
                    Some(d) if !local => Some(format!("ssh://git@{d}")),
                    _ if listen.ip().is_loopback() => Some(format!("ssh://git@{listen}")),
                    _ => None,
                };
                Some(SshConfig {
                    listen,
                    host_key: key,
                    public_url,
                })
            }
            None => None,
        };

        Ok(Config {
            server,
            storage,
            database,
            bus,
            web,
            github,
            ssh,
        })
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
