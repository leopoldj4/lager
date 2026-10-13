//! The web interface: browser sign-in, the live connection, and the built
//! files (docs/decisions/0014-web-interface.md).
//!
//! Everything lives under `/-/`, which no repository name can take:
//! `/-/api/session` signs in and out, `/-/api/live` is the WebSocket, and
//! `/-/assets/` holds the built files. Every other browser page that is no
//! Git request gets the application shell, which routes in the browser.

pub mod live;
pub mod read;
pub mod session;

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicUsize;

use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::sync::watch;

use crate::http::AppState;
use worktree_core::config::WebConfig;
use worktree_core::identity::{AuthError, TOKEN_PREFIX};

pub struct Web {
    pub sessions: session::Sessions,
    dir: Option<PathBuf>,
    /// Ends live connections when the server stops.
    stop: watch::Receiver<bool>,
    /// Open live connections.
    pub connections: AtomicUsize,
}

impl Web {
    /// `secure`: the server is reached over HTTPS (session cookies get
    /// `Secure`).
    pub fn new(cfg: &WebConfig, secure: bool, stop: watch::Receiver<bool>) -> Self {
        let dir = built_interface();
        match &dir {
            Some(d) => tracing::info!(dir = %d.display(), "serving the web interface"),
            None => tracing::info!("web interface not built; serving only its API"),
        }
        Self {
            sessions: session::Sessions::new(
                &cfg.session_secret,
                worktree_core::limits::SESSION_DAYS,
                secure,
            ),
            dir,
            stop,
            connections: AtomicUsize::new(0),
        }
    }

    /// Whether a browser request comes from this site: its `Origin` names
    /// the requested host. The Vite development server forwards the API
    /// with its own host, so it passes too. Browsers always send
    /// `Origin` on WebSocket upgrades and on POST requests, so another site
    /// cannot use the session cookie. Requests without `Origin` come from
    /// other programs, which hold no cookie of the user.
    pub fn origin_allowed(&self, headers: &HeaderMap) -> bool {
        let Some(origin) = headers.get(header::ORIGIN) else {
            return true;
        };
        let Ok(origin) = origin.to_str() else {
            return false;
        };
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
        let origin_host = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"));
        host.is_some() && origin_host == host
    }

    /// The application shell for browser pages, if the interface is built.
    pub async fn shell(&self) -> Option<Response> {
        let dir = self.dir.as_ref()?;
        let html = tokio::fs::read(dir.join("_shell.html")).await.ok()?;
        Some(
            (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-cache"),
                    (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
                    (header::REFERRER_POLICY, "same-origin"),
                ],
                html,
            )
                .into_response(),
        )
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/-/api/session", post(sign_in).delete(sign_out))
        .route("/-/api/live", get(live::upgrade))
        .route("/-/assets/{*file}", get(asset))
}

#[derive(serde::Deserialize)]
struct SignIn {
    user: String,
    password: String,
}

fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "code": code, "message": message })),
    )
        .into_response()
}

async fn sign_in(State(s): State<AppState>, headers: HeaderMap, body: Json<SignIn>) -> Response {
    let Some(web) = &s.web else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !web.origin_allowed(&headers) {
        s.registry.svc.metrics.inc("web_origin_rejections");
        return json_error(StatusCode::FORBIDDEN, "forbidden", "origin not allowed");
    }
    let SignIn { user, password } = body.0;
    if password.starts_with(TOKEN_PREFIX) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "sign in with your password, not an access token",
        );
    }
    let failed = || {
        s.registry.svc.metrics.inc("web_sign_in_failures");
        json_error(
            StatusCode::UNAUTHORIZED,
            "bad_credentials",
            "wrong user name or password",
        )
    };
    match s.directory.sign_in(&user, &password).await {
        Ok(_) => {}
        Err(AuthError::BadCredentials) => return failed(),
        Err(AuthError::Unavailable(m)) => {
            tracing::warn!(error = %m, "user lookup unavailable");
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "user lookup unavailable; retry later",
            );
        }
    }
    let found = match s.directory.lookup(&user).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return json_error(
                StatusCode::FORBIDDEN,
                "unsupported",
                "web sign-in needs users in the configuration file or the database",
            );
        }
        Err(_) => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "user lookup unavailable; retry later",
            );
        }
    };
    s.registry.svc.metrics.inc("web_sign_ins");
    tracing::info!(user = %user, "signed in on the web");
    let mut r = Json(serde_json::json!({ "user": user })).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&web.sessions.issue(&user, &found.stamp)).expect("valid"),
    );
    r
}

async fn sign_out(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let Some(web) = &s.web else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !web.origin_allowed(&headers) {
        return json_error(StatusCode::FORBIDDEN, "forbidden", "origin not allowed");
    }
    let mut r = StatusCode::NO_CONTENT.into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&web.sessions.clear()).expect("valid"),
    );
    r
}

/// A built file. Names carry a content hash, so they never change.
async fn asset(State(s): State<AppState>, UrlPath(file): UrlPath<String>) -> Response {
    let Some(dir) = s.web.as_ref().and_then(|w| w.dir.as_ref()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let rel = Path::new(&file);
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(data) = tokio::fs::read(dir.join("-/assets").join(rel)).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let kind = match rel.extension().and_then(|e| e.to_str()) {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, kind),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        data,
    )
        .into_response()
}

/// Whether a request is a page load of a browser, which gets the shell.
pub fn wants_page(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"))
}

/// The built interface (`nix build .#web`), installed beside the programs
/// in `share/worktree/web`. Without it, only the API under `/-/api/` answers.
fn built_interface() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?.join("share/worktree/web");
    dir.join("_shell.html").is_file().then_some(dir)
}
