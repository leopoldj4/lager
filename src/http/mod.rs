//! Git smart HTTP requests, identity checks, permissions, and resource limits.

pub mod access;
pub mod process;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::sync::Semaphore;

use self::access::{Directory, Identity};
use self::process::{Guards, Input, spawn_service};
use crate::config::{Access, RepoConfig};
use crate::git::hook;
use crate::repository::{PendingPush, PushAccess, Registry, RepoError};

pub struct AppInner {
    pub registry: Arc<Registry>,
    pub directory: Arc<Directory>,
    pub processes: Arc<Semaphore>,
    pub disk: Arc<DiskReservations>,
    pub routing: Option<Arc<crate::routing::Routing>>,
    pub socket_path: PathBuf,
    pub admin_api: bool,
}

pub type AppState = Arc<AppInner>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/_rustgit/health", get(|| async { "ok" }))
        .route("/_rustgit/metrics", get(metrics))
        .route("/_rustgit/notify", post(crate::routing::notify_handler))
        .route("/_rustgit/snapshot", post(snapshot))
        .fallback(git_handler)
        .with_state(state)
}

async fn metrics(State(s): State<AppState>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        s.registry.svc.metrics.render(),
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
    crate::config::validate_repo_name(name).ok()?;
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
        HeaderValue::from_static("Basic realm=\"rustgit\""),
    );
    r
}

fn repo_error(e: RepoError) -> Response {
    match e {
        RepoError::NotFound | RepoError::Deleted => {
            plain(StatusCode::NOT_FOUND, "repository not found")
        }
        RepoError::Unavailable(m) => {
            tracing::warn!(error = %m, "storage unavailable");
            plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage unavailable; retry later",
            )
        }
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
/// missing permission look the same to anonymous clients.
fn authorize(
    s: &AppInner,
    headers: &HeaderMap,
    name: &str,
    need: Access,
) -> Result<Authorized, Response> {
    s.directory.reload_if_changed();
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let identity = s.directory.authenticate(auth).map_err(|_| unauthorized())?;
    let Some(repo) = s.directory.repo(name) else {
        return Err(if identity == Identity::Anonymous {
            unauthorized()
        } else {
            plain(StatusCode::NOT_FOUND, "repository not found")
        });
    };
    let (level, _) = s.directory.access(&identity, name);
    if level < need {
        return Err(match (&identity, level) {
            (Identity::Anonymous, _) => unauthorized(),
            (_, Access::None) => plain(StatusCode::NOT_FOUND, "repository not found"),
            _ => plain(StatusCode::FORBIDDEN, "permission denied"),
        });
    }
    Ok(Authorized { repo, identity })
}

/// Only simple protocol requests are passed to Git.
fn git_protocol(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("git-protocol")?.to_str().ok()?;
    let ok = v.len() <= 256
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"=:.,-_".contains(&b));
    ok.then(|| v.to_string())
}

async fn process_permit(s: &AppInner) -> Result<tokio::sync::OwnedSemaphorePermit, Response> {
    match tokio::time::timeout(Duration::from_secs(10), s.processes.clone().acquire_owned()).await {
        Ok(Ok(p)) => Ok(p),
        _ => {
            s.registry.svc.metrics.inc("process_limit_rejections");
            Err(plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "server busy; retry later",
            ))
        }
    }
}

async fn git_handler(State(s): State<AppState>, req: Request) -> Response {
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
            rpc(&s, &parts.headers, &parts.uri, &name, service, body).await
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
    let auth = authorize(s, headers, name, service.needs())?;
    if let Err(r) =
        crate::routing::forward_or_keep(s, Method::GET, uri, headers, auth.repo.id, body).await
    {
        return Ok(r);
    }
    let controller = s.registry.controller(auth.repo.id);
    let view = controller.prepare_read().await.map_err(repo_error)?;
    let _permit = process_permit(s).await?;
    let protocol = git_protocol(headers);
    let mut cmd = s.registry.svc.git.command();
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
    let mut data = Vec::new();
    if !protocol.as_deref().unwrap_or("").contains("version=2") {
        data.extend(crate::git::pktline::encode(
            format!("# service={}\n", service.name()).as_bytes(),
        ));
        data.extend_from_slice(crate::git::pktline::FLUSH);
    }
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
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    name: &str,
    service: Service,
    body: Body,
) -> Result<Response, Response> {
    let auth = authorize(s, headers, name, service.needs())?;
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
    let mut cmd = s.registry.svc.git.command();
    let max_bytes;
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
            let reservation = s
                .disk
                .reserve(
                    content_length
                        .unwrap_or(limits.default_push_reservation_bytes)
                        .saturating_mul(2),
                )
                .map_err(|m| {
                    s.registry.svc.metrics.inc("disk_limit_rejections");
                    plain(StatusCode::INSUFFICIENT_STORAGE, &m)
                })?;
            guards.hold(reservation);
            let (cache, view) = controller.prepare_write().await.map_err(repo_error)?;
            let directory = s.directory.clone();
            let identity_name = auth.identity.name().to_string();
            let repo_name = auth.repo.name.clone();
            let who = auth.identity;
            let op = PendingPush {
                repo: auth.repo.id,
                user: identity_name,
                access: Arc::new(move || {
                    directory.reload_if_changed();
                    let (level, rules_version) = directory.access(&who, &repo_name);
                    PushAccess {
                        level,
                        rules_version,
                    }
                }),
                cache: cache.clone(),
            };
            let token = s.registry.register_push(op);
            cmd.arg("receive-pack")
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
    guards.hold(process_permit(s).await?);
    let body = spawn_service(
        cmd,
        Input {
            body,
            gzip,
            max_bytes,
        },
        guards,
        service.name(),
    )
    .map_err(|e| repo_error(RepoError::Internal(e.to_string())))?;
    Ok(git_response(
        format!("application/x-{}-result", service.name()),
        body,
    ))
}

#[derive(serde::Deserialize)]
struct SnapshotQuery {
    repo: String,
}

/// Maintenance through HTTP: creates a snapshot of the current generation.
async fn snapshot(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SnapshotQuery>,
) -> Response {
    if !s.admin_api {
        return plain(StatusCode::NOT_FOUND, "not found");
    }
    let auth = match authorize(&s, &headers, &q.repo, Access::Admin) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let controller = s.registry.controller(auth.repo.id);
    match crate::maintenance::snapshot(&controller, auth.identity.name()).await {
        Ok(r) => (StatusCode::OK, axum::Json(r)).into_response(),
        Err(e) => repo_error(e),
    }
}

/// Disk space reservations: work is rejected before the disk becomes full.
pub struct DiskReservations {
    dir: PathBuf,
    min_free: u64,
    reserved: Mutex<u64>,
    /// For tests: pretend that only this much space is free.
    pub free_override: Mutex<Option<u64>>,
}

pub struct DiskReservation {
    owner: Arc<DiskReservations>,
    bytes: u64,
}

impl Drop for DiskReservation {
    fn drop(&mut self) {
        *self.owner.reserved.lock().unwrap() -= self.bytes;
    }
}

impl DiskReservations {
    pub fn new(dir: &Path, min_free: u64) -> Arc<Self> {
        Arc::new(Self {
            dir: dir.to_path_buf(),
            min_free,
            reserved: Mutex::new(0),
            free_override: Mutex::new(None),
        })
    }

    pub fn reserve(self: &Arc<Self>, bytes: u64) -> Result<DiskReservation, String> {
        let free = match *self.free_override.lock().unwrap() {
            Some(f) => f,
            None => free_space(&self.dir).map_err(|e| format!("cannot check disk space: {e}"))?,
        };
        let mut reserved = self.reserved.lock().unwrap();
        if free.saturating_sub(*reserved).saturating_sub(bytes) < self.min_free {
            return Err("insufficient disk space on the server".into());
        }
        *reserved += bytes;
        Ok(DiskReservation {
            owner: self.clone(),
            bytes,
        })
    }
}

pub fn free_space(dir: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` is a valid out pointer.
    let r = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)]
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_paths_map_to_repository_names_and_services() {
        assert_eq!(
            parse_git_path("/demo.git/info/refs"),
            Some(GitRoute::InfoRefs("demo".into()))
        );
        assert_eq!(
            parse_git_path("/org/demo/info/refs"),
            Some(GitRoute::InfoRefs("org/demo".into()))
        );
        assert_eq!(
            parse_git_path("/demo/git-receive-pack"),
            Some(GitRoute::Rpc("demo".into(), Service::ReceivePack))
        );
        for bad in [
            "/../x/info/refs",
            "/a/../b/info/refs",
            "/.git/info/refs",
            "/demo/HEAD",
            "/demo/objects/aa/bb",
            "/info/refs",
        ] {
            assert_eq!(parse_git_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn disk_reservations_keep_the_minimum_free() {
        let d = DiskReservations::new(Path::new("/"), 100);
        *d.free_override.lock().unwrap() = Some(1000);
        let a = d.reserve(600).unwrap();
        assert!(d.reserve(400).is_err());
        drop(a);
        assert!(d.reserve(900).is_ok());
    }
}
