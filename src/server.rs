//! Server assembly: storage, Git, controllers, hook socket, and HTTP.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;

use crate::catalog::Catalog;
use crate::config::{CatalogKind, Config};
use crate::git::Git;
use crate::git::hook::{HookRequest, HookResponse, HookResult};
use crate::http::access::Directory;
use crate::http::limits::CustomerLimits;
use crate::http::{AppInner, AppState, DiskReservations};
use crate::metrics::Metrics;
use crate::model::branch_ref;
use crate::repository::{Registry, Services};
use crate::storage::{ObjectStoreBackend, Storage};

/// Storage from the configuration.
pub fn storage_from_config(cfg: &Config) -> anyhow::Result<Storage> {
    let backend = Arc::new(ObjectStoreBackend::s3(&cfg.storage.s3)?);
    Ok(Storage::new(backend, &cfg.storage.prefix)
        .with_upload_verification(cfg.storage.verify_uploads)
        .with_upload_limit(cfg.limits.max_concurrent_uploads))
}

pub struct RunningServer {
    pub addr: SocketAddr,
    pub state: AppState,
    http: JoinHandle<()>,
    hooks: JoinHandle<()>,
    /// Cache eviction and automatic snapshots.
    background: Vec<JoinHandle<()>>,
    /// Stops accepting HTTP connections and starting background work.
    stop_http: watch::Sender<bool>,
    /// Stops accepting push procedures from hooks.
    stop_hooks: watch::Sender<bool>,
    /// Running push procedures.
    pushes: TaskTracker,
    drain_delay: Duration,
    shutdown_timeout: Duration,
    socket_path: PathBuf,
}

impl RunningServer {
    pub fn registry(&self) -> &Arc<Registry> {
        &self.state.registry
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stops at once, like a crash: running requests end without an
    /// answer. Push procedures that have started are not cancelled; they
    /// end with the process. Accepted pushes are already in storage.
    pub fn stop(self) {
        self.http.abort();
        self.hooks.abort();
        for t in &self.background {
            t.abort();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }

    /// Stops cleanly. The health check fails first, for the drain delay,
    /// while requests are still served. Then the server accepts no new
    /// connections, finishes running requests and push procedures, and
    /// closes idle connections. What still runs at the shutdown timeout
    /// stops as in [`RunningServer::stop`].
    pub async fn shutdown(mut self) {
        self.state.health.start_draining();
        tracing::info!(
            delay_secs = self.drain_delay.as_secs(),
            "stopping: health check fails, requests are still served"
        );
        tokio::time::sleep(self.drain_delay).await;
        let deadline = tokio::time::Instant::now() + self.shutdown_timeout;
        tracing::info!("stopping: no new connections; waiting for running requests");
        let _ = self.stop_http.send(true);
        let http_done = tokio::time::timeout_at(deadline, &mut self.http)
            .await
            .is_ok();
        // Git processes of finished requests have ended, so no new push
        // procedure can start. Wait for procedures of clients that left.
        let _ = self.stop_hooks.send(true);
        let _ = tokio::time::timeout_at(deadline, &mut self.hooks).await;
        self.pushes.close();
        let hooks_done = tokio::time::timeout_at(deadline, self.pushes.wait())
            .await
            .is_ok();
        let mut background_done = true;
        for t in &mut self.background {
            background_done &= tokio::time::timeout_at(deadline, t).await.is_ok();
        }
        if http_done && hooks_done && background_done {
            tracing::info!("stopped");
        } else {
            tracing::warn!(
                requests = !http_done,
                pushes = !hooks_done,
                maintenance = !background_done,
                "stopping: shutdown timeout reached; ending remaining work"
            );
        }
        self.stop();
    }
}

/// Starts a server with the given storage. `config_path` enables reloading
/// of the repository and user table.
pub async fn start(
    cfg: Config,
    config_path: Option<PathBuf>,
    storage: Storage,
) -> anyhow::Result<RunningServer> {
    cfg.validate()?;
    let listener = TcpListener::bind(cfg.server.listen).await?;
    start_with_listener(cfg, config_path, storage, listener).await
}

pub async fn start_with_listener(
    cfg: Config,
    config_path: Option<PathBuf>,
    storage: Storage,
    listener: TcpListener,
) -> anyhow::Result<RunningServer> {
    let addr = listener.local_addr()?;
    let cache_root = cfg.server.cache_dir.clone();
    std::fs::create_dir_all(&cache_root)?;
    let hook_binary = match &cfg.server.hook_binary {
        Some(p) => p.clone(),
        None => std::env::current_exe()?,
    };
    let git = Git::new(
        cfg.server.git_binary.clone(),
        &cache_root.join("work"),
        &hook_binary,
    )?
    .with_process_limits(cfg.limits.git_cpu_secs, cfg.limits.git_memory_bytes);
    let version = git.version().await?;
    anyhow::ensure!(
        version >= crate::git::MIN_VERSION,
        "git {}.{} is too old",
        version.0,
        version.1
    );

    let metrics = Arc::new(Metrics::default());
    let svc = Arc::new(Services {
        storage,
        git,
        cache_root: cache_root.clone(),
        limits: cfg.limits.clone(),
        metrics,
        notifier: Mutex::new(None),
        disk: DiskReservations::new(&cache_root, cfg.limits.min_free_disk_bytes),
    });
    let registry = Registry::new(svc.clone())?;
    let identities = crate::identity::from_config(&cfg).await?;
    let mut directory = Directory::new(&cfg, config_path, identities);
    if cfg.catalog.kind == CatalogKind::Storage {
        directory = directory.with_catalog(Arc::new(Catalog::new(
            svc.storage.clone(),
            Duration::from_secs(cfg.catalog.cache_secs),
        )));
    }
    let directory = Arc::new(directory);

    for r in &cfg.repos {
        let branch = branch_ref(&r.default_branch)?;
        match crate::repository::admin::ensure_repository(&svc.storage, r.id, &branch).await {
            Ok(true) => tracing::info!(repo = %r.name, id = %r.id, "created repository"),
            Ok(false) => {}
            Err(e) => tracing::warn!(repo = %r.name, error = %e, "could not check repository head"),
        }
    }

    let routing = if cfg.routing.servers.is_empty() {
        None
    } else {
        let r = Arc::new(crate::routing::Routing::new(
            cfg.server.id.clone(),
            cfg.routing.clone(),
        )?);
        let rn = r.clone();
        *svc.notifier.lock().unwrap() = Some(Arc::new(move |repo, generation| {
            rn.notify(repo, generation)
        }));
        Some(r)
    };

    let socket_path = cfg.socket_path();
    let hook_listener = bind_socket(&socket_path)?;
    let (stop_http, _) = watch::channel(false);
    let (stop_hooks, _) = watch::channel(false);
    let reg = registry.clone();
    let stop = stop_hooks.subscribe();
    let pushes = TaskTracker::new();
    let tracker = pushes.clone();
    let hooks = tokio::spawn(async move { serve_hooks(reg, hook_listener, tracker, stop).await });

    let mut background = Vec::new();
    let reg = registry.clone();
    let idle = Duration::from_secs(cfg.limits.cache_idle_secs);
    let mut stop = stop_http.subscribe();
    background.push(tokio::spawn(async move {
        let mut tick =
            tokio::time::interval(Duration::from_secs(60).min(idle.max(Duration::from_secs(1))));
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stopped(&mut stop) => break,
            }
            let n = reg.evict_idle(idle).await;
            if n > 0 {
                tracing::info!(count = n, "removed idle caches");
            }
        }
    }));

    if cfg.maintenance.snapshot_after_entries > 0 {
        let reg = registry.clone();
        let routing = routing.clone();
        let after = cfg.maintenance.snapshot_after_entries;
        let every = Duration::from_secs(cfg.maintenance.check_interval_secs.max(1));
        let mut stop = stop_http.subscribe();
        background.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                // A snapshot that has started finishes before the loop ends.
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = stopped(&mut stop) => break,
                }
                crate::maintenance::auto_snapshot(&reg, after, |repo| {
                    routing.as_ref().is_none_or(|r| r.is_preferred_self(repo))
                })
                .await;
            }
        }));
    }

    let state: AppState = Arc::new(AppInner {
        registry: registry.clone(),
        directory,
        processes: Arc::new(Semaphore::new(cfg.limits.max_git_processes)),
        process_wait: Duration::from_secs(cfg.limits.process_wait_secs),
        customers: CustomerLimits::new(&cfg.customers),
        routing,
        socket_path: socket_path.clone(),
        health: crate::http::health::Health::new(svc.storage.clone()),
    });
    let app = crate::http::router(state.clone());
    let tls = match (&cfg.server.tls_certificate, &cfg.server.tls_private_key) {
        (Some(c), Some(k)) => Some(tls_acceptor(c, k)?),
        _ => None,
    };
    let idle_timeout = Duration::from_secs(cfg.limits.client_idle_timeout_secs);
    let stop = stop_http.subscribe();
    let http =
        tokio::spawn(async move { serve_http(listener, app, tls, idle_timeout, stop).await });
    tracing::info!(%addr, "rustgit listening");
    Ok(RunningServer {
        addr,
        state,
        http,
        hooks,
        background,
        stop_http,
        stop_hooks,
        pushes,
        drain_delay: Duration::from_secs(cfg.server.drain_delay_secs),
        shutdown_timeout: Duration::from_secs(cfg.server.shutdown_timeout_secs),
        socket_path,
    })
}

/// Waits for the stop signal. A dropped [`RunningServer`] is no signal:
/// the server then runs until the process ends, as before.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    if stop.wait_for(|s| *s).await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn bind_socket(path: &Path) -> anyhow::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    anyhow::ensure!(
        path.as_os_str().len() < crate::config::MAX_SOCKET_PATH,
        "socket path too long: {}",
        path.display()
    );
    if let Some(parent) = path.parent() {
        match std::fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => std::fs::create_dir_all(parent)?,
        }
        // A directory that another user prepared could expose the socket.
        let m = std::fs::symlink_metadata(parent)?;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        anyhow::ensure!(
            m.is_dir() && m.uid() == uid,
            "socket directory {} is not a directory owned by this user",
            parent.display()
        );
    }
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path)?;
    // Private: only this user can connect. Tokens protect each operation.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

async fn serve_hooks(
    registry: Arc<Registry>,
    listener: UnixListener,
    pushes: TaskTracker,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            a = listener.accept() => a,
            _ = stopped(&mut stop) => break,
        };
        let Ok((stream, _)) = accepted else {
            continue;
        };
        let reg = registry.clone();
        // Each request runs in its own task: a disconnected client or a
        // stopped server does not cancel a push after its CAS. A clean stop
        // waits for these tasks.
        pushes.spawn(async move {
            if let Err(e) = handle_hook(reg, stream).await {
                tracing::warn!(error = %e, "hook request failed");
            }
        });
    }
}

async fn handle_hook(registry: Arc<Registry>, stream: UnixStream) -> anyhow::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut line = String::new();
    BufReader::new(r.take(4 * 1024 * 1024))
        .read_line(&mut line)
        .await?;
    let req: HookRequest = serde_json::from_str(&line)?;
    let response = match registry.take_push(&req.token) {
        Some(op) => registry.controller(op.repo).push(&op, req.commands).await,
        None => HookResponse {
            results: req
                .commands
                .iter()
                .map(|c| HookResult {
                    name: c.name.clone(),
                    ok: false,
                    message: Some("invalid operation token".into()),
                })
                .collect(),
            messages: vec![],
        },
    };
    let mut data = serde_json::to_vec(&response)?;
    data.push(b'\n');
    w.write_all(&data).await?;
    w.shutdown().await?;
    Ok(())
}

fn tls_acceptor(cert: &Path, key: &Path) -> anyhow::Result<tokio_rustls::TlsAcceptor> {
    use tokio_rustls::rustls;
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(cert)?))
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(std::fs::File::open(key)?))?
        .ok_or_else(|| anyhow::anyhow!("no private key in {}", key.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

async fn serve_http(
    listener: TcpListener,
    app: axum::Router,
    tls: Option<tokio_rustls::TlsAcceptor>,
    idle: Duration,
    mut stop: watch::Receiver<bool>,
) {
    // Connections belong to this task: aborting it closes them. On a clean
    // stop, each connection finishes its running request and then closes.
    // Push procedures run in hook tasks and are not cancelled.
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            a = listener.accept() => a,
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            _ = stopped(&mut stop) => break,
        };
        let Ok((tcp, _)) = accepted else {
            continue;
        };
        let _ = tcp.set_nodelay(true);
        let app = app.clone();
        let tls = tls.clone();
        let stop = stop.clone();
        connections.spawn(async move {
            let result = match tls {
                Some(acceptor) => {
                    match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await
                    {
                        Ok(Ok(stream)) => serve_connection(stream, app, idle, stop).await,
                        _ => return,
                    }
                }
                None => serve_connection(tcp, app, idle, stop).await,
            };
            if let Err(e) = result {
                tracing::debug!(error = %e, "connection ended with error");
            }
        });
    }
    drop(listener);
    while connections.join_next().await.is_some() {}
}

async fn serve_connection<S>(
    stream: S,
    app: axum::Router,
    idle: Duration,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
    use hyper_util::server::conn::auto::Builder;
    use hyper_util::service::TowerToHyperService;
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(idle);
    let conn = builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app));
    tokio::pin!(conn);
    tokio::select! {
        r = conn.as_mut() => return r,
        _ = stopped(&mut stop) => {}
    }
    conn.as_mut().graceful_shutdown();
    conn.await
}
