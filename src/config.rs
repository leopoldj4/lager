//! Configuration file (TOML). The repository name table and development
//! credentials live here initially.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::RepoId;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub repos: Vec<RepoConfig>,
    #[serde(default)]
    pub users: Vec<UserConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    /// Identifies this server in the routing list.
    #[serde(default = "default_server_id")]
    pub id: String,
    pub cache_dir: PathBuf,
    /// Git program. A fixed, tested version should be used (see flake.nix).
    #[serde(default = "default_git")]
    pub git_binary: PathBuf,
    /// Program that Git runs as the proc-receive hook. Defaults to this program.
    #[serde(default)]
    pub hook_binary: Option<PathBuf>,
    /// Private Unix socket for hook requests. Defaults to a path in the cache directory.
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
    #[serde(default)]
    pub tls_certificate: Option<PathBuf>,
    #[serde(default)]
    pub tls_private_key: Option<PathBuf>,
    /// Permit plain HTTP on a non-loopback address because a load balancer
    /// terminates HTTPS in front of this server.
    #[serde(default)]
    pub tls_terminated_upstream: bool,
    /// Allow maintenance requests (snapshot) through HTTP for admin users.
    #[serde(default = "yes")]
    pub admin_api: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    #[serde(default)]
    pub kind: StorageKind,
    /// Key prefix inside the bucket.
    #[serde(default)]
    pub prefix: String,
    #[serde(default = "yes")]
    pub verify_uploads: bool,
    #[serde(default)]
    pub s3: Option<S3Config>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageKind {
    #[default]
    S3,
    /// In-process storage. Only for controlled tests: data is lost at exit.
    Memory,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default = "default_region")]
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default)]
    pub allow_http: bool,
    #[serde(default)]
    pub virtual_hosted_style: bool,
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
    /// Additional trusted CA certificate (PEM) for the storage endpoint.
    #[serde(default)]
    pub ca_certificate: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Maximum bytes in one push request body.
    pub max_receive_bytes: u64,
    /// Maximum simultaneous Git processes for client requests.
    pub max_git_processes: usize,
    /// Reject work that would leave less free disk space than this.
    pub min_free_disk_bytes: u64,
    /// Disk space reserved for a push without a Content-Length.
    pub default_push_reservation_bytes: u64,
    /// CAS attempts against newer heads before a retryable conflict.
    pub push_retry_limit: u32,
    /// Time limit for one push procedure, after the objects are received.
    pub push_deadline_secs: u64,
    /// Time limit to resolve an unknown CAS result.
    pub unknown_result_deadline_secs: u64,
    /// Close idle client connections after this time.
    pub client_idle_timeout_secs: u64,
    /// Remove caches that were not used for this time.
    pub cache_idle_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_receive_bytes: 2 * 1024 * 1024 * 1024,
            max_git_processes: 64,
            min_free_disk_bytes: 1024 * 1024 * 1024,
            default_push_reservation_bytes: 256 * 1024 * 1024,
            push_retry_limit: 8,
            push_deadline_secs: 120,
            unknown_result_deadline_secs: 30,
            client_idle_timeout_secs: 300,
            cache_idle_secs: 24 * 3600,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoutingConfig {
    /// Fixed server list. Empty means a single server.
    pub servers: Vec<PeerConfig>,
    /// Forward client requests to the preferred server for the repository.
    pub forward: bool,
    /// Tell other servers about accepted pushes. Lost notifications are harmless.
    pub notify: bool,
    /// Shared secret for requests between servers.
    pub peer_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    pub id: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub name: String,
    pub id: RepoId,
    #[serde(default = "default_branch")]
    pub default_branch: String,
    #[serde(default)]
    pub anonymous_read: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    None,
    Read,
    Write,
    /// Write plus force pushes (non-fast-forward branch updates, tag moves).
    Force,
    /// Force plus maintenance and repository management.
    Admin,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub name: String,
    /// Development credential in plain text. Use `password_sha256` elsewhere.
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub password_sha256: Option<String>,
    /// Repository name (or "*") to access level.
    #[serde(default)]
    pub access: BTreeMap<String, Access>,
}

fn yes() -> bool {
    true
}
fn default_server_id() -> String {
    "local".into()
}
fn default_git() -> PathBuf {
    PathBuf::from("git")
}
fn default_region() -> String {
    "us-east-1".into()
}
fn default_timeout() -> u64 {
    30
}
fn default_branch() -> String {
    "main".into()
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let mut names = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for r in &self.repos {
            validate_repo_name(&r.name)?;
            crate::model::branch_ref(&r.default_branch)?;
            anyhow::ensure!(
                names.insert(&r.name),
                "duplicate repository name {}",
                r.name
            );
            anyhow::ensure!(ids.insert(r.id), "duplicate repository id {}", r.id);
        }
        for u in &self.users {
            anyhow::ensure!(
                u.password.is_some() || u.password_sha256.is_some(),
                "user {} needs a password or password_sha256",
                u.name
            );
        }
        if self.storage.kind == StorageKind::S3 {
            anyhow::ensure!(
                self.storage.s3.is_some(),
                "storage.s3 is required for kind = \"s3\""
            );
        }
        let loopback = self.server.listen.ip().is_loopback();
        let tls = self.server.tls_certificate.is_some() && self.server.tls_private_key.is_some();
        anyhow::ensure!(
            loopback || tls || self.server.tls_terminated_upstream,
            "plain HTTP is permitted only on the loopback address; configure TLS"
        );
        Ok(())
    }

    pub fn socket_path(&self) -> PathBuf {
        self.server
            .socket_path
            .clone()
            .unwrap_or_else(|| self.server.cache_dir.join("hook.sock"))
    }
}

/// Repository names: segments of `[A-Za-z0-9._-]`, separated by `/`, never
/// starting with `.`. The name is never used as a disk path or storage key.
pub fn validate_repo_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty() && name.len() <= 200,
        "invalid repository name"
    );
    for seg in name.split('/') {
        anyhow::ensure!(
            !seg.is_empty()
                && !seg.starts_with('.')
                && !seg.ends_with(".git")
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid repository name {name:?}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_reject_paths_and_special_segments() {
        for ok in ["demo", "org/project", "a.b-c_d"] {
            assert!(validate_repo_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "../x",
            "a/../b",
            ".git",
            "a//b",
            "a/.hidden",
            "x.git",
            "a b",
            "a\\b",
            "/abs",
        ] {
            assert!(validate_repo_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn plain_http_on_a_public_address_is_rejected() {
        let text = r#"
            [server]
            listen = "0.0.0.0:8080"
            cache_dir = "/tmp/x"
            [storage]
            kind = "memory"
        "#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("loopback"));
        let local: Config = toml::from_str(&text.replace("0.0.0.0", "127.0.0.1")).unwrap();
        assert!(local.validate().is_ok());
    }
}
