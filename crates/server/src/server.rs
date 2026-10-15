//! Server assembly: storage, Git, controllers, hook socket, and HTTP.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;

use crate::disk::DiskReservations;
use crate::git::Git;
use crate::git::hook::{HookRequest, HookResponse, HookResult};
use crate::http::access::Directory;
use crate::http::{AppInner, AppState};
use crate::metrics::Metrics;
use crate::repository::{Registry, Services};
use worktree_core::catalog::Catalog;
use worktree_core::config::Config;
use worktree_core::limits;
use worktree_core::storage::Storage;

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
            delay_secs = limits::DRAIN_DELAY.as_secs(),
            "stopping: health check fails, requests are still served"
        );
        tokio::time::sleep(limits::DRAIN_DELAY).await;
        let deadline = tokio::time::Instant::now() + limits::SHUTDOWN_TIMEOUT;
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
        let _ = tokio::time::timeout_at(deadline, self.state.registry.svc.bus.flush()).await;
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

/// Starts a server with the given storage.
pub async fn start(cfg: Config, storage: Storage) -> anyhow::Result<RunningServer> {
    cfg.validate()?;
    let listener = TcpListener::bind(cfg.server.listen).await?;
    start_with_listener(cfg, storage, listener).await
}

pub async fn start_with_listener(
    cfg: Config,
    storage: Storage,
    listener: TcpListener,
) -> anyhow::Result<RunningServer> {
    let addr = listener.local_addr()?;
    let cache_root = cfg.server.cache_dir.clone();
    std::fs::create_dir_all(&cache_root)?;
    // Git comes from PATH; the container image takes it from Debian trixie.
    let git = Git::new(
        PathBuf::from("git"),
        &cache_root.join("work"),
        &std::env::current_exe()?,
    )?
    .with_process_limits(limits::GIT_CPU_SECS, limits::GIT_MEMORY_BYTES);
    let version = git.version().await?;
    anyhow::ensure!(
        version >= crate::git::MIN_VERSION,
        "git {}.{} is too old",
        version.0,
        version.1
    );

    let metrics = Arc::new(Metrics::default());
    let bus =
        Arc::new(crate::bus::Bus::connect(server_id(), cfg.bus.as_ref(), metrics.clone()).await?);
    let svc = Arc::new(Services {
        storage,
        git,
        cache_root: cache_root.clone(),
        metrics,
        bus: bus.clone(),
        disk: DiskReservations::new(&cache_root, limits::MIN_FREE_DISK_BYTES),
    });
    let registry = Registry::new(svc.clone())?;
    let users = worktree_core::identity::connect(&cfg).await?;
    let catalog = Catalog::new(svc.storage.clone(), limits::CATALOG_CACHE);
    let directory = Arc::new(Directory::new(users, catalog));

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
    let idle = limits::CACHE_IDLE;
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

    let (b, stop) = (bus.clone(), stop_http.subscribe());
    background.push(tokio::spawn(async move {
        if let Err(e) = b.receive(stop).await {
            tracing::error!(error = %e, "change events from other servers are not received");
        }
    }));

    // Another server accepted a push: bring the local cache up to date in
    // the background. Reads still check the storage head themselves.
    let reg = registry.clone();
    let mut changes = bus.subscribe();
    let mut stop = stop_http.subscribe();
    background.push(tokio::spawn(async move {
        loop {
            let change = tokio::select! {
                c = changes.recv() => c,
                _ = stopped(&mut stop) => break,
            };
            match change {
                Ok(c) if c.remote => {
                    let controller = reg.controller(c.repo);
                    tokio::spawn(async move {
                        let _ = controller.sync_to_storage().await;
                    });
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "cache updates skipped change events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }));

    let reg = registry.clone();
    let mut stop = stop_http.subscribe();
    background.push(tokio::spawn(async move {
        let mut tick = tokio::time::interval(limits::SNAPSHOT_CHECK_INTERVAL);
        loop {
            // A snapshot that has started finishes before the loop ends.
            tokio::select! {
                _ = tick.tick() => {}
                _ = stopped(&mut stop) => break,
            }
            crate::snapshot::auto_snapshot(&reg, limits::SNAPSHOT_AFTER_ENTRIES).await;
        }
    }));

    let state: AppState = Arc::new(AppInner {
        registry: registry.clone(),
        directory,
        processes: Arc::new(Semaphore::new(limits::MAX_GIT_PROCESSES)),
        socket_path: socket_path.clone(),
        health: crate::http::health::Health::new(svc.storage.clone(), bus),
        web: cfg.web.as_ref().map(|w| {
            // Off the loopback address, clients come through HTTPS.
            let secure = !cfg.server.listen.ip().is_loopback();
            Arc::new(crate::web::Web::new(w, secure, stop_http.subscribe()))
        }),
    });
    let app = crate::http::router(state.clone());
    let stop = stop_http.subscribe();
    let http =
        tokio::spawn(
            async move { serve_http(listener, app, limits::CLIENT_IDLE_TIMEOUT, stop).await },
        );
    tracing::info!(%addr, "worktree listening");
    Ok(RunningServer {
        addr,
        state,
        http,
        hooks,
        background,
        stop_http,
        stop_hooks,
        pushes,
        socket_path,
    })
}

/// Waits for the stop signal. A dropped [`RunningServer`] is no signal:
/// the server then runs until the process ends, as before.
pub(crate) async fn stopped(stop: &mut watch::Receiver<bool>) {
    if stop.wait_for(|s| *s).await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn bind_socket(path: &Path) -> anyhow::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    anyhow::ensure!(
        path.as_os_str().len() < worktree_core::config::MAX_SOCKET_PATH,
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

/// Names this server in change events and its NATS connection: the host
/// name. It need not be unique.
fn server_id() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    match std::str::from_utf8(&buf[..end]) {
        Ok(name) if ok && !name.is_empty() => name.to_string(),
        _ => "worktree".into(),
    }
}

async fn serve_http(
    listener: TcpListener,
    app: axum::Router,
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
        let stop = stop.clone();
        connections.spawn(async move {
            if let Err(e) = serve_connection(tcp, app, idle, stop).await {
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
    // Upgrades: the live connection of the web interface. Upgraded
    // connections end on their own stop signal.
    let conn =
        builder.serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(app));
    tokio::pin!(conn);
    tokio::select! {
        r = conn.as_mut() => return r,
        _ = stopped(&mut stop) => {}
    }
    conn.as_mut().graceful_shutdown();
    conn.await
}
