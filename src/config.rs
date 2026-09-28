//! Configuration file (TOML). The repository name table and development
//! credentials live here initially. Production
//! setups move the name table to the repository catalog in storage and
//! check identities with salted password hashes or an identity service.

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
    #[serde(default)]
    pub customers: Vec<CustomerConfig>,
    #[serde(default)]
    pub identity: Option<IdentityConfig>,
    #[serde(default)]
    pub catalog: CatalogConfig,
    #[serde(default)]
    pub maintenance: MaintenanceConfig,
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
    /// Largest single Git object in a push, after decompression.
    pub max_object_bytes: u64,
    /// Total size of the new objects in a push, after decompression.
    pub max_expanded_bytes: u64,
    /// CPU seconds for one Git process of a client request. 0: no limit.
    pub git_cpu_secs: u64,
    /// Address space of one Git process of a client request (Linux). 0: no limit.
    pub git_memory_bytes: u64,
    /// Time a request waits for a free Git process before a busy response.
    pub process_wait_secs: u64,
    /// Simultaneous pack uploads to storage.
    pub max_concurrent_uploads: usize,
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
            max_object_bytes: 1024 * 1024 * 1024,
            max_expanded_bytes: 8 * 1024 * 1024 * 1024,
            git_cpu_secs: 0,
            git_memory_bytes: 0,
            process_wait_secs: 10,
            max_concurrent_uploads: 16,
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

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub name: String,
    pub id: RepoId,
    #[serde(default = "default_branch")]
    pub default_branch: String,
    #[serde(default)]
    pub anonymous_read: bool,
    /// The customer that owns the repository, for customer limits.
    #[serde(default)]
    pub customer: Option<String>,
    #[serde(default)]
    pub ref_rules: Vec<RefRule>,
}

impl RepoConfig {
    pub fn new(name: &str, id: RepoId) -> Self {
        Self {
            name: name.into(),
            id,
            default_branch: default_branch(),
            anonymous_read: false,
            customer: None,
            ref_rules: vec![],
        }
    }
}

/// The access levels that changes to matching refs need.
///
/// Without a matching rule, creation, fast-forward updates, and deletion
/// need `write`; non-fast-forward updates and tag moves need `force`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RefRule {
    /// A complete ref name, or a prefix that ends with `*`.
    pub pattern: String,
    /// Creation and fast-forward updates.
    #[serde(default = "access_write")]
    pub update: Access,
    /// Non-fast-forward updates and tag moves.
    #[serde(default = "access_force")]
    pub force: Access,
    #[serde(default = "access_write")]
    pub delete: Access,
}

impl RefRule {
    fn matches(&self, name: &str) -> bool {
        match self.pattern.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == self.pattern,
        }
    }
}

pub fn validate_ref_rule(rule: &RefRule) -> anyhow::Result<()> {
    let body = rule.pattern.strip_suffix('*').unwrap_or(&rule.pattern);
    anyhow::ensure!(
        body.starts_with("refs/") && !body.contains('*'),
        "invalid ref rule pattern {:?}: use a ref name or a prefix ending in *",
        rule.pattern
    );
    Ok(())
}

/// The most specific rule for a ref: an exact name, else the longest prefix.
pub fn ref_rule<'a>(rules: &'a [RefRule], name: &str) -> Option<&'a RefRule> {
    rules
        .iter()
        .filter(|r| r.matches(name))
        .max_by_key(|r| (!r.pattern.ends_with('*'), r.pattern.len()))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustomerConfig {
    pub name: String,
    /// Simultaneous Git processes for this customer's repositories.
    #[serde(default)]
    pub max_git_processes: Option<usize>,
    /// Git requests per minute.
    #[serde(default)]
    pub max_requests_per_minute: Option<u64>,
    /// Received push bytes per hour.
    #[serde(default)]
    pub max_receive_bytes_per_hour: Option<u64>,
}

/// An external identity service. The server sends
/// `{"username": ..., "password": ...}` and expects
/// `{"user": ..., "access": {"<repository or *>": "<level>"}}` with status 200,
/// or 401/403 for wrong credentials.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityConfig {
    pub url: String,
    /// Sent as a bearer token to the identity service.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "default_identity_cache")]
    pub cache_secs: u64,
    #[serde(default = "default_identity_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub ca_certificate: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CatalogKind {
    /// Repository names from the `[[repos]]` table of this file.
    #[default]
    Config,
    /// Repository names from the catalog in the storage service.
    Storage,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct CatalogConfig {
    pub kind: CatalogKind,
    /// A name lookup is reused for this time. Deleted repositories are
    /// never served: each request also checks the repository head.
    pub cache_secs: u64,
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self {
            kind: CatalogKind::Config,
            cache_secs: 5,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct MaintenanceConfig {
    /// Create a snapshot when this many entries follow the last snapshot.
    /// 0 disables automatic snapshots. Only the preferred server for a
    /// repository starts them.
    pub snapshot_after_entries: u64,
    pub check_interval_secs: u64,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            snapshot_after_entries: 0,
            check_interval_secs: 60,
        }
    }
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
    /// Development credential in plain text. Only on the loopback address.
    #[serde(default)]
    pub password: Option<String>,
    /// Unsalted SHA-256 of the password. Only on the loopback address.
    #[serde(default)]
    pub password_sha256: Option<String>,
    /// Salted hash from `rustgit hash-password` (PBKDF2-HMAC-SHA256).
    #[serde(default)]
    pub password_hash: Option<String>,
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
fn default_identity_cache() -> u64 {
    60
}
fn default_identity_timeout() -> u64 {
    5
}
fn access_write() -> Access {
    Access::Write
}
fn access_force() -> Access {
    Access::Force
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
            for rule in &r.ref_rules {
                validate_ref_rule(rule)?;
            }
            if let Some(c) = &r.customer {
                anyhow::ensure!(
                    self.customers.iter().any(|x| &x.name == c),
                    "repository {} names unknown customer {c}",
                    r.name
                );
            }
        }
        let loopback = self.server.listen.ip().is_loopback();
        for u in &self.users {
            anyhow::ensure!(
                u.password.is_some() || u.password_sha256.is_some() || u.password_hash.is_some(),
                "user {} needs a password_hash (or a password for local development)",
                u.name
            );
            anyhow::ensure!(
                loopback || (u.password.is_none() && u.password_sha256.is_none()),
                "user {}: development credentials (password, password_sha256) are permitted \
                 only on the loopback address; use password_hash",
                u.name
            );
            if let Some(h) = &u.password_hash {
                crate::http::password::PasswordHash::parse(h)
                    .map_err(|e| anyhow::anyhow!("user {}: {e}", u.name))?;
            }
        }
        if self.catalog.kind == CatalogKind::Storage {
            anyhow::ensure!(
                self.repos.is_empty(),
                "with catalog.kind = \"storage\", repositories live in the catalog; remove [[repos]]"
            );
        }
        if self.storage.kind == StorageKind::S3 {
            anyhow::ensure!(
                self.storage.s3.is_some(),
                "storage.s3 is required for kind = \"s3\""
            );
        }
        let tls = self.server.tls_certificate.is_some() && self.server.tls_private_key.is_some();
        anyhow::ensure!(
            loopback || tls || self.server.tls_terminated_upstream,
            "plain HTTP is permitted only on the loopback address; configure TLS"
        );
        Ok(())
    }

    /// The hook socket. Unix socket paths are short (about 100 bytes), so a
    /// default path in a deep cache directory moves to a private directory
    /// under the system temporary directory, named after the cache directory.
    pub fn socket_path(&self) -> PathBuf {
        if let Some(p) = &self.server.socket_path {
            return p.clone();
        }
        let default = self.server.cache_dir.join("hook.sock");
        if default.as_os_str().len() < MAX_SOCKET_PATH {
            return default;
        }
        use sha2::Digest;
        let digest = sha2::Sha256::digest(self.server.cache_dir.as_os_str().as_encoded_bytes());
        std::env::temp_dir()
            .join(format!("rustgit-{}", &hex::encode(digest)[..16]))
            .join("hook.sock")
    }
}

/// Longest Unix socket path that works on Linux and macOS.
pub const MAX_SOCKET_PATH: usize = 100;

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

    /// The configuration examples in README.md load and pass validation.
    #[test]
    fn readme_configuration_examples_are_valid() {
        let readme = include_str!("../README.md");
        let blocks: Vec<&str> = readme
            .split("```toml\n")
            .skip(1)
            .map(|b| b.split("```").next().unwrap())
            .collect();
        assert_eq!(blocks.len(), 2);
        let production: Config = toml::from_str(blocks[0]).unwrap();
        production.validate().unwrap();
        assert_eq!(production.catalog.kind, CatalogKind::Storage);
        assert_eq!(production.routing.servers.len(), 3);
        let local = format!(
            "[server]\nlisten = \"127.0.0.1:1\"\ncache_dir = \"/tmp/x\"\n\
             [storage]\nkind = \"memory\"\n[[customers]]\nname = \"acme\"\n{}",
            blocks[1]
        );
        let repos: Config = toml::from_str(&local).unwrap();
        repos.validate().unwrap();
        let rules = &repos.repos[0].ref_rules;
        assert_eq!(
            ref_rule(rules, "refs/heads/main").unwrap().force,
            Access::Admin
        );
        assert_eq!(
            ref_rule(rules, "refs/heads/release/1").unwrap().update,
            Access::Admin
        );
        assert!(ref_rule(rules, "refs/heads/other").is_none());
    }
}
