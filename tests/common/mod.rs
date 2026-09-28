//! Test harness: real servers with shared, controllable storage, and
//! standard Git clients.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use rustgit::config::{
    Access, Config, Limits, RepoConfig, RoutingConfig, ServerConfig, StorageConfig, StorageKind,
    UserConfig,
};
use rustgit::model::{Head, LogEntry, Operation, RefState, RepoId};
use rustgit::server::RunningServer;
use rustgit::storage::fault::FaultBackend;
use rustgit::storage::{ObjectStoreBackend, Storage};
use tempfile::TempDir;

pub const REPO: &str = "demo";

/// Users and their access to every repository.
pub const USERS: &[(&str, Access)] = &[
    ("admin", Access::Admin),
    ("alice", Access::Force),
    ("bob", Access::Write),
    ("carol", Access::Read),
    ("dave", Access::None),
];

pub fn password(user: &str) -> String {
    format!("{user}-secret")
}

/// Shared storage and a temporary directory for several servers and clients.
pub struct Env {
    pub dir: TempDir,
    pub fault: Arc<FaultBackend>,
    pub storage: Storage,
    pub repo: RepoId,
    pub limits: Limits,
    servers: std::sync::atomic::AtomicUsize,
}

impl Env {
    pub fn new() -> Env {
        Self::with_limits(test_limits())
    }

    /// Shared storage on another backend, for example a provider.
    pub fn on_backend(backend: Arc<dyn rustgit::storage::Backend>, prefix: &str) -> Env {
        let mut env = Self::new();
        env.fault = Arc::new(FaultBackend::new(backend));
        env.storage = Storage::new(env.fault.clone(), prefix);
        env
    }

    pub fn with_limits(limits: Limits) -> Env {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_env("RUSTGIT_TEST_LOG")
                    .unwrap_or_else(|_| "off".into()),
            )
            .with_test_writer()
            .try_init();
        let fault = Arc::new(FaultBackend::new(Arc::new(ObjectStoreBackend::in_memory())));
        let storage = Storage::new(fault.clone(), "test");
        Env {
            dir: tempfile::Builder::new()
                .prefix("rg")
                .tempdir_in("/tmp")
                .unwrap(),
            fault,
            storage,
            repo: RepoId::new(),
            limits,
            servers: Default::default(),
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn config(&self, cache_dir: &Path) -> Config {
        Config {
            server: ServerConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                id: "test".into(),
                cache_dir: cache_dir.to_path_buf(),
                git_binary: server_git(),
                hook_binary: Some(PathBuf::from(env!("CARGO_BIN_EXE_rustgit"))),
                socket_path: None,
                tls_certificate: None,
                tls_private_key: None,
                tls_terminated_upstream: false,
                admin_api: true,
            },
            storage: StorageConfig {
                kind: StorageKind::Memory,
                prefix: "test".into(),
                verify_uploads: true,
                s3: None,
            },
            limits: self.limits.clone(),
            routing: RoutingConfig::default(),
            repos: vec![RepoConfig::new(REPO, self.repo)],
            users: USERS
                .iter()
                .map(|(name, level)| UserConfig {
                    name: (*name).into(),
                    password: Some(password(name)),
                    password_sha256: None,
                    password_hash: None,
                    access: BTreeMap::from([("*".to_string(), *level)]),
                })
                .collect(),
            customers: vec![],
            identity: None,
            catalog: Default::default(),
            maintenance: Default::default(),
        }
    }

    /// Starts a server with a new, empty cache directory on the shared storage.
    pub async fn start(&self) -> Server {
        let n = self
            .servers
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.start_with(|_| {}, &format!("s{n}")).await
    }

    pub async fn start_with(&self, edit: impl FnOnce(&mut Config), cache: &str) -> Server {
        self.start_on(self.storage.clone(), edit, cache).await
    }

    pub async fn start_on(
        &self,
        storage: Storage,
        edit: impl FnOnce(&mut Config),
        cache: &str,
    ) -> Server {
        let cache_dir = self.path(cache);
        let mut cfg = self.config(&cache_dir);
        edit(&mut cfg);
        let running = rustgit::server::start(cfg, None, storage)
            .await
            .expect("server starts (set RUSTGIT_TEST_GIT to Git 2.45 or later)");
        Server { running, cache_dir }
    }

    /// A Git client with its own home directory.
    pub fn client(&self, name: &str) -> Client {
        let home = self.path(&format!("client-{name}"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".gitconfig"),
            format!(
                "[user]\n\tname = {name}\n\temail = {name}@example.com\n\
                 [init]\n\tdefaultBranch = main\n\
                 [advice]\n\tdetachedHead = false\n"
            ),
        )
        .unwrap();
        Client { home }
    }

    pub async fn head(&self) -> Head {
        load_head(&self.storage, self.repo).await
    }

    /// The refs that the stored history produces.
    pub async fn stored_refs(&self) -> BTreeMap<String, String> {
        stored_refs(&self.storage, self.repo).await
    }

    /// All entries of the current history, oldest first.
    pub async fn history(&self) -> Vec<LogEntry> {
        let head = self.head().await;
        let mut out = Vec::new();
        let mut next = Some(head.latest_entry);
        while let Some(id) = next {
            let e = self.storage.get_entry(self.repo, id).await.unwrap();
            next = e.previous;
            out.push(e);
        }
        out.reverse();
        out
    }

    pub async fn push_entries(&self) -> Vec<LogEntry> {
        self.history()
            .await
            .into_iter()
            .filter(|e| e.operation == Operation::Push)
            .collect()
    }
}

/// Git for the server. It needs a newer version than many systems have, so
/// `RUSTGIT_TEST_GIT` can name it. Clients use `git` from `PATH`.
pub fn server_git() -> PathBuf {
    std::env::var_os("RUSTGIT_TEST_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| "git".into())
}

pub fn test_limits() -> Limits {
    Limits {
        min_free_disk_bytes: 0,
        push_deadline_secs: 30,
        unknown_result_deadline_secs: 2,
        client_idle_timeout_secs: 30,
        ..Limits::default()
    }
}

pub async fn load_head(storage: &Storage, repo: RepoId) -> Head {
    storage.load_head(repo).await.unwrap().expect("head").0
}

pub async fn stored_refs(storage: &Storage, repo: RepoId) -> BTreeMap<String, String> {
    let state = stored_state(storage, repo).await;
    state
        .refs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub async fn stored_state(storage: &Storage, repo: RepoId) -> RefState {
    let head = load_head(storage, repo).await;
    let chain = rustgit::repository::sync::load_chain(storage, &head)
        .await
        .unwrap();
    rustgit::repository::sync::chain_state(&chain).unwrap()
}

pub struct Server {
    pub running: RunningServer,
    pub cache_dir: PathBuf,
}

impl Server {
    pub fn url_as(&self, user: &str) -> String {
        format!(
            "http://{user}:{}@{}/{REPO}.git",
            password(user),
            self.running.addr
        )
    }

    /// URL for a user with force permission.
    pub fn url(&self) -> String {
        self.url_as("alice")
    }

    pub fn anonymous_url(&self) -> String {
        format!("http://{}/{REPO}.git", self.running.addr)
    }

    pub fn base(&self) -> String {
        self.running.url()
    }

    /// Stops the server and removes its complete cache.
    pub fn stop_and_remove_cache(self) {
        let dir = self.cache_dir.clone();
        self.running.stop();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

pub struct Client {
    pub home: PathBuf,
}

impl Client {
    pub fn command(&self, dir: &Path) -> tokio::process::Command {
        let mut c = tokio::process::Command::new("git");
        c.current_dir(dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/bin/false")
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        c
    }

    pub async fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.command(dir).args(args).output().await.unwrap()
    }

    /// Runs Git and requires success. Returns trimmed stdout.
    pub async fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.run(dir, args).await;
        assert!(
            out.status.success(),
            "git {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Runs Git and requires failure. Returns stderr.
    pub async fn fails(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.run(dir, args).await;
        assert!(
            !out.status.success(),
            "git {args:?} succeeded unexpectedly: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    pub async fn clone(&self, url: &str, name: &str) -> PathBuf {
        let dir = self.home.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        self.ok(&self.home, &["clone", "--quiet", url, name]).await;
        dir
    }

    /// A new local repository with `origin` set to `url`.
    pub async fn init(&self, url: &str, name: &str) -> PathBuf {
        let dir = self.home.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        self.ok(&dir, &["init", "--quiet"]).await;
        self.ok(&dir, &["remote", "add", "origin", url]).await;
        dir
    }

    /// Writes files and commits them. Returns the commit OID.
    pub async fn commit(&self, dir: &Path, files: &[(&str, &[u8])], message: &str) -> String {
        for (name, data) in files {
            let p = dir.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, data).unwrap();
        }
        self.ok(dir, &["add", "--all"]).await;
        self.ok(dir, &["commit", "--quiet", "--allow-empty", "-m", message])
            .await;
        self.rev(dir, "HEAD").await
    }

    pub async fn rev(&self, dir: &Path, rev: &str) -> String {
        self.ok(dir, &["rev-parse", rev]).await
    }

    /// Remote refs as reported by the server, without `HEAD`.
    pub async fn ls_remote(&self, url: &str) -> BTreeMap<String, String> {
        let out = self.ok(&self.home, &["ls-remote", url]).await;
        parse_ls_remote(&out)
    }

    /// Local refs under refs/remotes/origin and refs/tags of a clone, in
    /// the server's naming.
    pub async fn clone_refs(&self, dir: &Path) -> BTreeMap<String, String> {
        let out = self
            .ok(dir, &["for-each-ref", "--format=%(objectname) %(refname)"])
            .await;
        out.lines()
            .filter_map(|l| {
                let (oid, name) = l.split_once(' ')?;
                if let Some(b) = name.strip_prefix("refs/remotes/origin/") {
                    (b != "HEAD").then(|| (format!("refs/heads/{b}"), oid.to_string()))
                } else if name.starts_with("refs/tags/") {
                    Some((name.to_string(), oid.to_string()))
                } else {
                    None
                }
            })
            .collect()
    }
}

pub fn parse_ls_remote(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| {
            let (oid, name) = l.split_once('\t')?;
            (name != "HEAD" && !name.ends_with("^{}")).then(|| (name.to_string(), oid.to_string()))
        })
        .collect()
}

/// Deterministic, incompressible bytes.
pub fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Text lines that compress and delta well.
pub fn text(lines: usize, tag: &str) -> Vec<u8> {
    (0..lines)
        .map(|i| format!("line {i} of the {tag} file with some stable text\n"))
        .collect::<String>()
        .into_bytes()
}

/// The contents of every tracked file at a revision of a clone.
pub async fn tree_files(client: &Client, dir: &Path, rev: &str) -> HashMap<String, Vec<u8>> {
    let list = client.ok(dir, &["ls-tree", "-r", "--name-only", rev]).await;
    let mut out = HashMap::new();
    for name in list.lines() {
        let o = client
            .command(dir)
            .args(["cat-file", "blob", &format!("{rev}:{name}")])
            .output()
            .await
            .unwrap();
        assert!(o.status.success());
        out.insert(name.to_string(), o.stdout);
    }
    out
}

/// What a client learned about its push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushResult {
    Accepted,
    Rejected,
    /// A timeout or unknown result. Not a definite failure.
    Unknown,
}

pub async fn push_result(client: &Client, dir: &Path, args: &[&str]) -> PushResult {
    let out = client.run(dir, args).await;
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        PushResult::Accepted
    } else if err.contains("result unknown") {
        PushResult::Unknown
    } else {
        PushResult::Rejected
    }
}

/// Checks the stored history against the transaction rules and the results
/// that clients received. `pushes` pairs a pushed commit with its result.
pub async fn check_history(env: &Env, pushes: &[(String, PushResult)]) {
    let history = env.history().await;
    assert_eq!(history[0].generation, 0);
    let mut state = RefState::default();
    let mut requests = std::collections::HashSet::new();
    for (i, e) in history.iter().enumerate() {
        assert_eq!(e.generation, i as u64, "generations are contiguous");
        // Every entry applies to the state before it: each expected old
        // value matched, so no accepted change was lost or applied twice.
        state
            .apply_entry(e)
            .expect("entry applies to the previous state");
        if e.operation == Operation::Push {
            assert!(requests.insert(e.request), "request applied twice");
        }
    }
    assert_eq!(
        state
            .refs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        env.stored_refs().await,
        "the head's view (with snapshots) equals the complete history"
    );
    for (oid, result) in pushes {
        let n = history
            .iter()
            .flat_map(|e| &e.refs)
            .filter(|u| u.new.as_ref().map(|o| o.as_str()) == Some(oid.as_str()))
            .count();
        match result {
            PushResult::Accepted => assert_eq!(n, 1, "accepted push {oid} is in the history once"),
            PushResult::Rejected => assert_eq!(n, 0, "rejected push {oid} is in the history"),
            PushResult::Unknown => assert!(n <= 1),
        }
    }
}

impl Env {
    /// Starts `n` servers with a fixed server list (`s0`, `s1`, ...) and
    /// separate caches on the shared storage.
    pub async fn cluster(&self, n: usize, edit: impl Fn(&mut Config)) -> Vec<Server> {
        let k = self
            .servers
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut listeners = Vec::new();
        for _ in 0..n {
            listeners.push(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
        }
        let peers: Vec<rustgit::config::PeerConfig> = listeners
            .iter()
            .enumerate()
            .map(|(i, l)| rustgit::config::PeerConfig {
                id: format!("s{i}"),
                url: format!("http://{}", l.local_addr().unwrap()),
            })
            .collect();
        let mut out = Vec::new();
        for (i, l) in listeners.into_iter().enumerate() {
            let cache_dir = self.path(&format!("c{k}-{i}"));
            let mut cfg = self.config(&cache_dir);
            cfg.server.id = format!("s{i}");
            cfg.routing.servers = peers.clone();
            cfg.routing.peer_token = Some("peer-secret".into());
            edit(&mut cfg);
            let running = rustgit::server::start_with_listener(cfg, None, self.storage.clone(), l)
                .await
                .unwrap();
            out.push(Server { running, cache_dir });
        }
        out
    }
}

impl Server {
    pub fn svc(&self) -> &Arc<rustgit::repository::Services> {
        &self.running.registry().svc
    }

    pub fn metric(&self, name: &str) -> u64 {
        self.svc().metrics.get(name)
    }

    pub fn url_for(&self, user: &str, repo: &str) -> String {
        format!(
            "http://{user}:{}@{}/{repo}.git",
            password(user),
            self.running.addr
        )
    }
}

/// A load balancer that sends each HTTP request, not each connection, to
/// the next server. One Git operation then uses several servers.
pub struct Balancer {
    pub addr: std::net::SocketAddr,
    /// The server index for each request, in order.
    pub served: Arc<std::sync::Mutex<Vec<usize>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Balancer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Balancer {
    pub async fn start(servers: &[&Server]) -> Balancer {
        use axum::body::Body;
        use axum::extract::{Request, State};
        use futures::TryStreamExt;
        #[derive(Clone)]
        struct St {
            backends: Arc<Vec<std::net::SocketAddr>>,
            next: Arc<std::sync::atomic::AtomicUsize>,
            served: Arc<std::sync::Mutex<Vec<usize>>>,
            client: reqwest::Client,
        }
        async fn forward(State(st): State<St>, req: Request) -> axum::response::Response {
            let i = st.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % st.backends.len();
            st.served.lock().unwrap().push(i);
            let (parts, body) = req.into_parts();
            let url = format!(
                "http://{}{}",
                st.backends[i],
                parts
                    .uri
                    .path_and_query()
                    .map(|p| p.as_str())
                    .unwrap_or("/")
            );
            let mut r = st.client.request(parts.method.clone(), url);
            for (k, v) in &parts.headers {
                if !matches!(k.as_str(), "host" | "connection" | "transfer-encoding") {
                    r = r.header(k, v);
                }
            }
            let r = r.body(reqwest::Body::wrap_stream(body.into_data_stream()));
            match r.send().await {
                Ok(resp) => {
                    let mut out =
                        axum::response::Response::builder().status(resp.status().as_u16());
                    for (k, v) in resp.headers() {
                        if !matches!(
                            k.as_str(),
                            "connection" | "transfer-encoding" | "content-length"
                        ) {
                            out = out.header(k.as_str(), v.as_bytes());
                        }
                    }
                    out.body(Body::from_stream(
                        resp.bytes_stream().map_err(std::io::Error::other),
                    ))
                    .unwrap()
                }
                Err(_) => axum::response::Response::builder()
                    .status(502)
                    .body(Body::empty())
                    .unwrap(),
            }
        }
        let served = Arc::new(std::sync::Mutex::new(Vec::new()));
        let st = St {
            backends: Arc::new(servers.iter().map(|s| s.running.addr).collect()),
            next: Default::default(),
            served: served.clone(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        };
        let app = axum::Router::new().fallback(forward).with_state(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Balancer { addr, served, task }
    }

    pub fn url_as(&self, user: &str) -> String {
        format!("http://{user}:{}@{}/{REPO}.git", password(user), self.addr)
    }

    /// The distinct servers that handled requests.
    pub fn servers_used(&self) -> std::collections::BTreeSet<usize> {
        self.served.lock().unwrap().iter().copied().collect()
    }
}

/// One pkt-line.
pub fn pkt(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", data.len() + 4).into_bytes();
    out.extend_from_slice(data);
    out
}

/// A receive-pack request: commands (`old new name`), then the pack.
pub fn receive_request(commands: &[(&str, &str, &str)], pack: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    for (i, (old, new, name)) in commands.iter().enumerate() {
        let caps = if i == 0 { "\0report-status" } else { "" };
        body.extend(pkt(format!("{old} {new} {name}{caps}\n").as_bytes()));
    }
    body.extend_from_slice(b"0000");
    body.extend_from_slice(pack);
    body
}

/// Sends a raw receive-pack request. Returns the status and the report.
pub async fn raw_receive_pack(server: &Server, user: &str, body: Vec<u8>) -> (u16, String) {
    let r = reqwest::Client::new()
        .post(format!("{}/{REPO}.git/git-receive-pack", server.base()))
        .basic_auth(user, Some(password(user)))
        .header("content-type", "application/x-git-receive-pack-request")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&r.bytes().await.unwrap()).into_owned(),
    )
}

impl Client {
    /// A pack of the objects that `revs` (one per line, `^` excludes) selects.
    pub async fn pack(&self, dir: &Path, revs: &str) -> Vec<u8> {
        use tokio::io::AsyncWriteExt;
        let mut child = self
            .command(dir)
            .args(["pack-objects", "--revs", "--stdout", "-q"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(revs.as_bytes()).await.unwrap();
        drop(stdin);
        let out = child.wait_with_output().await.unwrap();
        assert!(out.status.success());
        out.stdout
    }
}

pub const ZERO: &str = "0000000000000000000000000000000000000000";

/// Starts a fetch of `want` and returns without reading the response: a
/// slow client. The server keeps a Git process and a read view for it.
pub async fn start_slow_fetch(server: &Server, user: &str, want: &str) -> reqwest::Response {
    let mut body = pkt(format!("want {want}\n").as_bytes());
    body.extend_from_slice(b"0000");
    body.extend(pkt(b"done\n"));
    let r = reqwest::Client::new()
        .post(format!("{}/{REPO}.git/git-upload-pack", server.base()))
        .basic_auth(user, Some(password(user)))
        .header("content-type", "application/x-git-upload-pack-request")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    r
}
