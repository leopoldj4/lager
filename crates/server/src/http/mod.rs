//! Git smart HTTP requests, identity checks, permissions, and resource limits.

pub mod access;
pub mod health;
pub mod limits;
pub mod process;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::sync::Semaphore;
use tracing::Instrument;

use self::access::{AuthError, Directory, Identity};
use self::limits::{CustomerLimits, LimitExceeded};
use self::process::{Guards, Input, spawn_service};
use crate::git::hook;
use crate::repository::{PendingPush, PushAccess, Registry, RepoError};
use worktree_core::catalog::CatalogError;
use worktree_core::config::{Access, RepoConfig};
use worktree_core::model::RequestId;

pub const REQUEST_ID_HEADER: &str = "x-worktree-request-id";
/// Set on every response while the server stops. Other servers then stop
/// sending it requests.
pub const DRAINING_HEADER: &str = "x-worktree-draining";

pub struct AppInner {
    pub registry: Arc<Registry>,
    pub directory: Arc<Directory>,
    pub processes: Arc<Semaphore>,
    pub process_wait: Duration,
    pub customers: CustomerLimits,
    pub routing: Option<Arc<crate::routing::Routing>>,
    pub socket_path: PathBuf,
    pub health: health::Health,
}

pub type AppState = Arc<AppInner>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/_worktree/health", get(ready))
        .route("/_worktree/live", get(|| async { "ok\n" }))
        .route("/_worktree/metrics", get(metrics))
        .route("/_worktree/notify", post(crate::routing::notify_handler))
        .fallback(git_handler)
        .layer(middleware::from_fn_with_state(state.clone(), request_id))
        .with_state(state)
}

/// Readiness for a load balancer: 503 while the storage service does not
/// answer or while the server stops. `/_worktree/live` only shows that the
/// process runs.
async fn ready(State(s): State<AppState>) -> Response {
    use health::NotReady;
    match s.health.check().await {
        Ok(()) => "ok\n".into_response(),
        Err(NotReady::Draining) => (StatusCode::SERVICE_UNAVAILABLE, "draining\n").into_response(),
        Err(NotReady::StorageUnavailable) => {
            s.registry.svc.metrics.inc("health_storage_failures");
            (StatusCode::SERVICE_UNAVAILABLE, "storage unavailable\n").into_response()
        }
    }
}

/// The identifier of one HTTP request, for diagnostic records. A push uses
/// it as its request UUID, so the log entry and the server log agree.
#[derive(Clone, Copy, Debug)]
pub struct RequestTag(pub uuid::Uuid);

/// Gives each request an identifier, a log span, and one summary record.
/// Credentials and contents are never logged.
async fn request_id(State(s): State<AppState>, mut req: Request, next: Next) -> Response {
    let id = uuid::Uuid::new_v4();
    req.extensions_mut().insert(RequestTag(id));
    let span = tracing::info_span!("request", id = %id);
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = Instant::now();
    let mut resp = next.run(req).instrument(span.clone()).await;
    resp.headers_mut().insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_str(&id.to_string()).expect("uuid"),
    );
    if s.health.is_draining() {
        resp.headers_mut()
            .insert(DRAINING_HEADER, HeaderValue::from_static("1"));
    }
    span.in_scope(|| {
        tracing::info!(
            %method,
            path,
            status = resp.status().as_u16(),
            ms = start.elapsed().as_millis() as u64,
            "request"
        )
    });
    resp
}

async fn metrics(State(s): State<AppState>) -> Response {
    use std::sync::atomic::Ordering::Relaxed;
    let svc = &s.registry.svc;
    let mut text = svc.metrics.render();
    let st = svc.storage.stats();
    for (name, v) in [
        ("storage_pack_uploads_total", st.pack_uploads.load(Relaxed)),
        (
            "storage_uploaded_bytes_total",
            st.uploaded_bytes.load(Relaxed),
        ),
        (
            "storage_pack_downloads_total",
            st.pack_downloads.load(Relaxed),
        ),
        (
            "storage_downloaded_bytes_total",
            st.downloaded_bytes.load(Relaxed),
        ),
    ] {
        text.push_str(&format!("worktree_{name} {v}\n"));
    }
    let mut caches = 0;
    for c in s.registry.controllers() {
        caches += c.cached_generation().await.is_some() as u64;
    }
    text.push_str(&format!("worktree_caches {caches}\n"));
    text.push_str(&format!(
        "worktree_draining {}\n",
        s.health.is_draining() as u8
    ));
    text.push_str(&format!(
        "worktree_git_processes_in_use {}\n",
        svc.limits.max_git_processes - s.processes.available_permits()
    ));
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        text,
    )
        .into_response()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
        }
    }
    fn subcommand(self) -> &'static str {
        match self {
            Service::UploadPack => "upload-pack",
            Service::ReceivePack => "receive-pack",
        }
    }
    fn parse(s: &str) -> Option<Service> {
        match s {
            "git-upload-pack" => Some(Service::UploadPack),
            "git-receive-pack" => Some(Service::ReceivePack),
            _ => None,
        }
    }
    fn needs(self) -> Access {
        match self {
            Service::UploadPack => Access::Read,
            Service::ReceivePack => Access::Write,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum GitRoute {
    InfoRefs(String),
    Rpc(String, Service),
}

/// Maps a URL path to a repository name and Git service. The name is only a
/// key into the repository table, never a disk path.
fn parse_git_path(path: &str) -> Option<GitRoute> {
    let path = path.strip_prefix('/')?;
    let (name, route) = if let Some(n) = path.strip_suffix("/info/refs") {
        (n, None)
    } else if let Some(n) = path.strip_suffix("/git-upload-pack") {
        (n, Some(Service::UploadPack))
    } else {
        (
            path.strip_suffix("/git-receive-pack")?,
            Some(Service::ReceivePack),
        )
    };
    let name = name.strip_suffix(".git").unwrap_or(name);
    worktree_core::config::validate_repo_name(name).ok()?;
    Some(match route {
        None => GitRoute::InfoRefs(name.to_string()),
        Some(s) => GitRoute::Rpc(name.to_string(), s),
    })
}

fn plain(status: StatusCode, msg: &str) -> Response {
    let mut r = (status, format!("{msg}\n")).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

fn unauthorized() -> Response {
    let mut r = plain(StatusCode::UNAUTHORIZED, "authentication required");
    r.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"worktree\""),
    );
    r
}

fn unavailable(what: &str, m: &str) -> Response {
    tracing::warn!(error = %m, "{what} unavailable");
    plain(
        StatusCode::SERVICE_UNAVAILABLE,
        &format!("{what} unavailable; retry later"),
    )
}

fn too_many(e: LimitExceeded, metrics: &crate::metrics::Metrics) -> Response {
    metrics.inc("customer_limit_rejections");
    let mut r = plain(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("customer limit reached: {}", e.0),
    );
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    r
}

fn repo_error(e: RepoError) -> Response {
    match e {
        RepoError::NotFound | RepoError::Deleted => {
            plain(StatusCode::NOT_FOUND, "repository not found")
        }
        RepoError::Unavailable(m) => unavailable("storage", &m),
        RepoError::NoSpace(m) => plain(StatusCode::INSUFFICIENT_STORAGE, &m),
        RepoError::Internal(m) => {
            tracing::error!(error = %m, "internal error");
            plain(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

struct Authorized {
    repo: RepoConfig,
    identity: Identity,
}

/// Checks identity and repository permission. Unknown repositories and
/// missing permission look the same to anonymous clients. With `create`, a
/// missing repository is created if the user may (`catalog.create_on_push`).
async fn authorize(
    s: &AppInner,
    headers: &HeaderMap,
    name: &str,
    need: Access,
    create: bool,
) -> Result<Authorized, Response> {
    s.directory.reload_if_changed();
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let identity = match s.directory.authenticate(auth).await {
        Ok(i) => i,
        Err(AuthError::BadCredentials) => return Err(unauthorized()),
        Err(AuthError::Unavailable(m)) => return Err(unavailable("user lookup", &m)),
    };
    let repo = s
        .directory
        .repo(name)
        .await
        .map_err(|e| repo_error(e.into()))?;
    let repo = match repo {
        None if create && s.directory.may_create_on_push(&identity, name) => {
            Some(create_on_push(s, &identity, name).await?)
        }
        repo => repo,
    };
    let Some(repo) = repo else {
        return Err(if identity == Identity::Anonymous {
            unauthorized()
        } else {
            plain(StatusCode::NOT_FOUND, "repository not found")
        });
    };
    let (level, _) = s.directory.access(&identity, &repo);
    if level < need {
        return Err(match (&identity, level) {
            (Identity::Anonymous, _) => unauthorized(),
            (_, Access::None) => plain(StatusCode::NOT_FOUND, "repository not found"),
            _ => plain(StatusCode::FORBIDDEN, "permission denied"),
        });
    }
    s.customers
        .admit_request(repo.customer.as_deref())
        .map_err(|e| too_many(e, &s.registry.svc.metrics))?;
    Ok(Authorized { repo, identity })
}

/// Creates a missing repository for a push. Both requests of a push can get
/// here: a server with an older lookup finds the repository that another
/// server created.
async fn create_on_push(
    s: &AppInner,
    identity: &Identity,
    name: &str,
) -> Result<RepoConfig, Response> {
    let catalog = s
        .directory
        .catalog()
        .expect("checked by may_create_on_push");
    match catalog.ensure(name, identity.name()).await {
        Ok((repo, created)) => {
            if created {
                s.registry.svc.metrics.inc("repositories_created_on_push");
                tracing::info!(repo = %name, id = %repo.id, user = %identity.name(), "created repository on push");
            }
            Ok(repo)
        }
        Err(CatalogError::Storage(e)) => Err(repo_error(e.into())),
        Err(CatalogError::Invalid(m)) => Err(plain(StatusCode::BAD_REQUEST, &m)),
        Err(e) => Err(unavailable("repository catalog", &e.to_string())),
    }
}

/// Only simple protocol requests are passed to Git.
///
/// Protocol version 2 is never passed on: its `upload-pack` accepts any
/// object ID in a want and ignores `uploadpack.allow*InWant`. The cache
/// holds objects of rejected pushes, so reads use protocol version 0 or 1,
/// which accept only wants reachable from the advertised refs.
fn git_protocol(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("git-protocol")?.to_str().ok()?;
    let ok = v.len() <= 256
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"=:.,-_".contains(&b));
    let kept: Vec<&str> = v
        .split(':')
        .filter(|p| !p.starts_with("version=") || *p == "version=1")
        .collect();
    (ok && !kept.is_empty()).then(|| kept.join(":"))
}

/// A server process slot and, for customers with a limit, a customer slot.
async fn process_permit(s: &AppInner, repo: &RepoConfig) -> Result<Guards, Response> {
    let mut guards = Guards::default();
    let customer = s
        .customers
        .process_permit(repo.customer.as_deref())
        .map_err(|e| too_many(e, &s.registry.svc.metrics))?;
    if let Some(p) = customer {
        guards.hold(p);
    }
    match tokio::time::timeout(s.process_wait, s.processes.clone().acquire_owned()).await {
        Ok(Ok(p)) => {
            guards.hold(p);
            Ok(guards)
        }
        _ => {
            s.registry.svc.metrics.inc("process_limit_rejections");
            let mut r = plain(StatusCode::SERVICE_UNAVAILABLE, "server busy; retry later");
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            Err(r)
        }
    }
}

async fn git_handler(State(s): State<AppState>, req: Request) -> Response {
    let tag = req
        .extensions()
        .get::<RequestTag>()
        .copied()
        .unwrap_or(RequestTag(uuid::Uuid::new_v4()));
    let (parts, body) = req.into_parts();
    let Some(route) = parse_git_path(parts.uri.path()) else {
        return plain(StatusCode::NOT_FOUND, "not found");
    };
    let result = match (&parts.method, route) {
        (&Method::GET, GitRoute::InfoRefs(name)) => {
            let service = parts
                .uri
                .query()
                .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("service=")))
                .and_then(Service::parse);
            let Some(service) = service else {
                // The dumb protocol is not supported.
                return plain(StatusCode::FORBIDDEN, "smart HTTP required");
            };
            info_refs(&s, &parts.headers, &parts.uri, &name, service, body).await
        }
        (&Method::POST, GitRoute::Rpc(name, service)) => {
            rpc(&s, tag, &parts.headers, &parts.uri, &name, service, body).await
        }
        _ => Err(plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")),
    };
    result.unwrap_or_else(|r| r)
}

async fn info_refs(
    s: &AppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    name: &str,
    service: Service,
    body: Body,
) -> Result<Response, Response> {
    let create = service == Service::ReceivePack;
    let auth = authorize(s, headers, name, service.needs(), create).await?;
    if let Err(r) =
        crate::routing::forward_or_keep(s, Method::GET, uri, headers, auth.repo.id, body).await
    {
        return Ok(r);
    }
    s.registry.svc.metrics.inc("ref_advertisements");
    let controller = s.registry.controller(auth.repo.id);
    let view = controller.prepare_read().await.map_err(repo_error)?;
    let _permits = process_permit(s, &auth.repo).await?;
    let protocol = git_protocol(headers);
    let mut cmd = s.registry.svc.git.service_command();
    cmd.arg(service.subcommand())
        .arg("--stateless-rpc")
        .arg("--advertise-refs")
        .arg(&view.dir);
    if let Some(p) = &protocol {
        cmd.env("GIT_PROTOCOL", p);
    }
    let out = crate::git::run(&mut cmd, None)
        .await
        .map_err(|e| repo_error(RepoError::Internal(e.to_string())))?;
    let mut data =
        crate::git::pktline::encode(format!("# service={}\n", service.name()).as_bytes());
    data.extend_from_slice(crate::git::pktline::FLUSH);
    data.extend(out);
    Ok(git_response(
        format!("application/x-{}-advertisement", service.name()),
        Body::from(data),
    ))
}

fn git_response(content_type: String, body: Body) -> Response {
    let mut r = Response::new(body);
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).expect("valid"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, max-age=0, must-revalidate"),
    );
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    h.insert(
        header::EXPIRES,
        HeaderValue::from_static("Fri, 01 Jan 1980 00:00:00 GMT"),
    );
    r
}

async fn rpc(
    s: &AppState,
    tag: RequestTag,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    name: &str,
    service: Service,
    body: Body,
) -> Result<Response, Response> {
    let create = service == Service::ReceivePack;
    let auth = authorize(s, headers, name, service.needs(), create).await?;
    let expected = format!("application/x-{}-request", service.name());
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some(expected.as_str())
    {
        return Err(plain(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unexpected content type",
        ));
    }
    let gzip = match headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
    {
        None | Some("identity") => false,
        Some("gzip") | Some("x-gzip") => true,
        Some(_) => {
            return Err(plain(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported content encoding",
            ));
        }
    };
    let body =
        match crate::routing::forward_or_keep(s, Method::POST, uri, headers, auth.repo.id, body)
            .await
        {
            Ok(body) => body,
            Err(forwarded) => return Ok(forwarded),
        };
    let limits = &s.registry.svc.limits;
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut guards = Guards::default();
    let controller = s.registry.controller(auth.repo.id);
    let mut cmd = s.registry.svc.git.service_command();
    s.registry.svc.metrics.inc(match service {
        Service::UploadPack => "upload_pack_requests",
        Service::ReceivePack => "receive_pack_requests",
    });
    let max_bytes;
    let mut budget = None;
    match service {
        Service::UploadPack => {
            // Negotiation requests are small; the limit protects memory and disk.
            max_bytes = 64 * 1024 * 1024;
            let view = controller.prepare_read().await.map_err(repo_error)?;
            cmd.arg("upload-pack").arg("--stateless-rpc").arg(&view.dir);
            guards.hold(view);
        }
        Service::ReceivePack => {
            max_bytes = limits.max_receive_bytes;
            if content_length.is_some_and(|n| n > max_bytes) {
                return Err(plain(StatusCode::PAYLOAD_TOO_LARGE, "push too large"));
            }
            let expected = content_length.unwrap_or(limits.default_push_reservation_bytes);
            budget = s
                .customers
                .receive_budget(auth.repo.customer.as_deref(), content_length.unwrap_or(0))
                .map_err(|e| too_many(e, &s.registry.svc.metrics))?;
            let reservation = s
                .registry
                .svc
                .disk
                .reserve(expected.saturating_mul(2))
                .map_err(|m| {
                    s.registry.svc.metrics.inc("disk_limit_rejections");
                    plain(StatusCode::INSUFFICIENT_STORAGE, &m)
                })?;
            guards.hold(reservation);
            let (cache, view) = controller.prepare_write().await.map_err(repo_error)?;
            let op = PendingPush {
                repo: auth.repo.id,
                user: auth.identity.name().to_string(),
                request: RequestId(tag.0),
                access: push_access(
                    s.directory.clone(),
                    auth.identity.clone(),
                    auth.repo.name.clone(),
                ),
                cache: cache.clone(),
                default_branch_changed: default_branch_changed(
                    s.directory.clone(),
                    auth.repo.name.clone(),
                ),
            };
            let token = s.registry.register_push(op);
            // Git also stops reading at the receive limit.
            cmd.arg("-c")
                .arg(format!("receive.maxInputSize={max_bytes}"))
                .arg("receive-pack")
                .arg("--stateless-rpc")
                .arg(&cache.dir)
                .env(hook::ENV_SOCKET, &s.socket_path)
                .env(hook::ENV_TOKEN, &token.token);
            guards.hold(token);
            guards.hold(cache);
            guards.hold(view);
        }
    }
    if let Some(p) = git_protocol(headers) {
        cmd.env("GIT_PROTOCOL", p);
    }
    guards.hold(process_permit(s, &auth.repo).await?);
    let body = spawn_service(
        cmd,
        Input {
            body,
            gzip,
            max_bytes,
            budget,
        },
        guards,
        service.name(),
        s.registry.svc.metrics.clone(),
    )
    .map_err(|e| repo_error(RepoError::Internal(e.to_string())))?;
    Ok(git_response(
        format!("application/x-{}-result", service.name()),
        body,
    ))
}

/// The permission check that the push procedure repeats before each
/// attempt: the configuration file or catalog can change in between.
fn push_access(
    directory: Arc<Directory>,
    who: Identity,
    repo_name: String,
) -> crate::repository::AccessCheck {
    Arc::new(move || {
        let (directory, who, repo_name) = (directory.clone(), who.clone(), repo_name.clone());
        Box::pin(async move {
            directory.reload_if_changed();
            let repo = directory
                .repo(&repo_name)
                .await
                .map_err(|e| e.to_string())?;
            let Some(repo) = repo else {
                return Ok(PushAccess::none());
            };
            let (level, rules_version) = directory.access(&who, &repo);
            Ok(PushAccess {
                repo: repo.id,
                level,
                rules_version,
                ref_rules: repo.ref_rules,
            })
        })
    })
}

/// Keeps the default branch in the catalog record equal to the history.
fn default_branch_changed(
    directory: Arc<Directory>,
    repo_name: String,
) -> Option<crate::repository::DefaultBranchChanged> {
    directory.catalog()?;
    Some(Arc::new(move |id, branch| {
        let (directory, repo_name) = (directory.clone(), repo_name.clone());
        Box::pin(async move {
            let catalog = directory.catalog().expect("checked");
            let Some(short) = branch.as_str().strip_prefix("refs/heads/") else {
                return;
            };
            let result = catalog
                .update(&repo_name, |r| {
                    // The name may map to a newer repository by now.
                    if r.repository == id {
                        r.default_branch = short.to_string();
                    }
                })
                .await;
            if let Err(e) = result {
                tracing::warn!(repo = %repo_name, error = %e, "catalog default branch not changed");
            }
        })
    }))
}
