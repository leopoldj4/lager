//! Server assembly: storage, Git, controllers, hook socket, and HTTP.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::catalog::Catalog;
use crate::config::{CatalogKind, Config, StorageKind};
use crate::git::Git;
use crate::git::hook::{HookRequest, HookResponse, HookResult};
use crate::http::access::Directory;
use crate::http::limits::CustomerLimits;
use crate::http::{AppInner, AppState, DiskReservations};
use crate::metrics::Metrics;
use crate::model::branch_ref;
use crate::repository::{Registry, Services};
use crate::storage::{ObjectStoreBackend, Storage};

/// Storage from the configuration. `memory` storage is new and empty.
pub fn storage_from_config(cfg: &Config) -> anyhow::Result<Storage> {
    let backend: Arc<dyn crate::storage::Backend> = match cfg.storage.kind {
        StorageKind::Memory => Arc::new(ObjectStoreBackend::in_memory()),
        StorageKind::S3 => Arc::new(ObjectStoreBackend::s3(
            cfg.storage.s3.as_ref().expect("validated"),
        )?),
    };
    Ok(Storage::new(backend, &cfg.storage.prefix)
        .with_upload_verification(cfg.storage.verify_uploads)
        .with_upload_limit(cfg.limits.max_concurrent_uploads))
}

pub struct RunningServer {
    pub addr: SocketAddr,
    pub state: AppState,
    tasks: Vec<JoinHandle<()>>,
    socket_path: PathBuf,
}

impl RunningServer {
    pub fn registry(&self) -> &Arc<Registry> {
        &self.state.registry
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stops accepting requests. Accepted pushes are already in storage.
    pub fn stop(self) {
        for t in &self.tasks {
            t.abort();
        }
        let _ = std::fs::remove_file(&self.socket_path);
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
        failpoints: Arc::new(crate::failpoint::Failpoints::from_env()),
        disk: DiskReservations::new(&cache_root, cfg.limits.min_free_disk_bytes),
    });
    let registry = Registry::new(svc.clone())?;
    let mut directory = Directory::new(&cfg, config_path)?;
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
    let mut tasks = Vec::new();
    let reg = registry.clone();
    tasks.push(tokio::spawn(async move {
        serve_hooks(reg, hook_listener).await
    }));

    let reg = registry.clone();
    let idle = Duration::from_secs(cfg.limits.cache_idle_secs);
    tasks.push(tokio::spawn(async move {
        let mut tick =
            tokio::time::interval(Duration::from_secs(60).min(idle.max(Duration::from_secs(1))));
        loop {
            tick.tick().await;
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
        tasks.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
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
        admin_api: cfg.server.admin_api,
    });
    let app = crate::http::router(state.clone());
    let tls = match (&cfg.server.tls_certificate, &cfg.server.tls_private_key) {
        (Some(c), Some(k)) => Some(tls_acceptor(c, k)?),
        _ => None,
    };
    let idle_timeout = Duration::from_secs(cfg.limits.client_idle_timeout_secs);
    tasks.push(tokio::spawn(async move {
        serve_http(listener, app, tls, idle_timeout).await
    }));
    tracing::info!(%addr, "rustgit listening");
    Ok(RunningServer {
        addr,
        state,
        tasks,
        socket_path,
    })
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

async fn serve_hooks(registry: Arc<Registry>, listener: UnixListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let reg = registry.clone();
        // Each request runs in its own task: a disconnected client does not
        // cancel a push after its CAS.
        tokio::spawn(async move {
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
) {
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
    use hyper_util::server::conn::auto::Builder;
    use hyper_util::service::TowerToHyperService;
    // Connections belong to this task: stopping the server closes them.
    // Push procedures run in hook tasks and are not cancelled.
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            a = listener.accept() => a,
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
        };
        let Ok((tcp, _)) = accepted else {
            continue;
        };
        let _ = tcp.set_nodelay(true);
        let service = TowerToHyperService::new(app.clone());
        let tls = tls.clone();
        connections.spawn(async move {
            let mut builder = Builder::new(TokioExecutor::new());
            builder
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(idle);
            let result = match tls {
                Some(acceptor) => {
                    match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await
                    {
                        Ok(Ok(stream)) => {
                            builder
                                .serve_connection(TokioIo::new(stream), service)
                                .await
                        }
                        _ => return,
                    }
                }
                None => builder.serve_connection(TokioIo::new(tcp), service).await,
            };
            if let Err(e) = result {
                tracing::debug!(error = %e, "connection ended with error");
            }
        });
    }
}
