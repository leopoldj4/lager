//! Connecting a GitHub account, and the GitHub queries of the settings page
//! (docs/decisions/0020-github-mirrors.md).
//!
//! `GET /-/api/github/connect` sends the signed-in user to GitHub with a
//! random state in a short-lived cookie; GitHub sends them back to
//! `GET /-/api/github/callback`, which checks the state, keeps the token,
//! and returns to `/-/settings/github`. Both are page loads, not API calls.
//!
//! `POST /-/api/github/hook/<repository UUID>` takes a mirror's webhook
//! deliveries (docs/decisions/0027-live-github-mirrors.md).

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query as UrlQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};

use super::read::{Query, ReadError};
use crate::github::{Delivery, GithubError};
use crate::http::{AppInner, AppState};
use lager_core::identity::Identity;
use lager_core::limits;
use lager_core::model::RepoId;
use lager_core::password::constant_time_eq;

const COOKIE: &str = "lager_github_state";
const COOKIE_PATH: &str = "/-/api/github";
const SETTINGS: &str = "/-/settings/github";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/-/api/github/connect", get(connect))
        .route("/-/api/github/callback", get(callback))
        .route(
            "/-/api/github/hook/{repo}",
            post(hook).layer(DefaultBodyLimit::max(limits::GITHUB_HOOK_MAX_BYTES)),
        )
}

/// A webhook delivery from GitHub: pokes the mirror if the signature
/// matches.
async fn hook(
    State(s): State<AppState>,
    Path(repo): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (Some(github), Ok(id)) = (&s.github, repo.parse::<uuid::Uuid>()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    };
    let event = header("x-github-event");
    match github
        .receive(RepoId(id), event, header("x-hub-signature-256"), &body)
        .await
    {
        Ok(Delivery::Accepted) => (StatusCode::ACCEPTED, "sync requested\n").into_response(),
        Ok(Delivery::Pong) => (StatusCode::OK, "pong\n").into_response(),
        Ok(Delivery::Unknown) => (StatusCode::NOT_FOUND, "no such mirror\n").into_response(),
        Ok(Delivery::BadSignature) => {
            tracing::info!(%repo, event, "webhook delivery with a wrong signature");
            (StatusCode::UNAUTHORIZED, "wrong signature\n").into_response()
        }
        Err(e) => {
            tracing::warn!(%repo, error = %e, "webhook delivery not handled");
            (StatusCode::SERVICE_UNAVAILABLE, "retry later\n").into_response()
        }
    }
}

fn redirect(to: &str, cookie: Option<String>) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    let h = r.headers_mut();
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(to).unwrap_or(HeaderValue::from_static(SETTINGS)),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(c) = cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
        h.insert(header::SET_COOKIE, c);
    }
    r
}

fn settings_error(message: &str, cookie: Option<String>) -> Response {
    redirect(
        &format!(
            "{SETTINGS}?github_error={}",
            utf8_percent_encode(message, NON_ALPHANUMERIC)
        ),
        cookie,
    )
}

fn state_cookie(s: &AppState, value: &str, max_age: u32) -> String {
    let secure = s.web.as_ref().is_some_and(|w| w.sessions.secure());
    format!(
        "{COOKIE}={value}; Path={COOKIE_PATH}; Max-Age={max_age}; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

async fn signed_in(s: &AppState, headers: &HeaderMap) -> Result<Option<String>, Response> {
    let web = s
        .web
        .as_ref()
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    Ok(match web.identity(&s.directory, headers).await? {
        Identity::User { name, .. } => Some(name),
        Identity::Anonymous => None,
    })
}

/// Sends the user to GitHub to grant access.
async fn connect(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let user = match signed_in(&s, &headers).await {
        Ok(Some(u)) => u,
        Ok(None) => return redirect("/-/sign-in?next=%2F-%2Fsettings%2Fgithub", None),
        Err(r) => return r,
    };
    let Some(github) = &s.github else {
        return settings_error("GitHub is not configured on this server", None);
    };
    let state = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let callback = s
        .web
        .as_ref()
        .and_then(|w| w.public_origin.as_deref())
        .map(|o| format!("{}/-/api/github/callback", o.trim_end_matches('/')));
    tracing::debug!(user, "connecting GitHub");
    redirect(
        &github.authorize_url(&state, callback.as_deref()),
        Some(state_cookie(&s, &state, 600)),
    )
}

#[derive(serde::Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error_description: Option<String>,
}

/// GitHub sends the user back here.
async fn callback(
    State(s): State<AppState>,
    headers: HeaderMap,
    UrlQuery(q): UrlQuery<Callback>,
) -> Response {
    let clear = Some(state_cookie(&s, "", 0));
    let user = match signed_in(&s, &headers).await {
        Ok(Some(u)) => u,
        Ok(None) => return redirect("/-/sign-in?next=%2F-%2Fsettings%2Fgithub", clear),
        Err(r) => return r,
    };
    let Some(github) = &s.github else {
        return settings_error("GitHub is not configured on this server", clear);
    };
    if let Some(m) = q.error_description {
        return settings_error(&m, clear);
    }
    let expected = super::session::cookie_value(&headers, COOKIE);
    let state_ok = match (&expected, &q.state) {
        (Some(e), Some(got)) => constant_time_eq(e.as_bytes(), got.as_bytes()),
        _ => false,
    };
    let Some(code) = q.code.filter(|_| state_ok) else {
        return settings_error("the GitHub sign-in expired; connect again", clear);
    };
    match github.finish_connect(&user, &code).await {
        Ok(_) => redirect(SETTINGS, clear),
        Err(e) => {
            tracing::warn!(user, error = %e, "GitHub account not connected");
            settings_error(&e.to_string(), clear)
        }
    }
}

impl From<GithubError> for ReadError {
    fn from(e: GithubError) -> Self {
        match e {
            GithubError::SignedOut => Self::new("unauthorized", "sign in first"),
            GithubError::NotConnected => Self::new("not_connected", e.to_string()),
            GithubError::NotFound(m) => Self::new("not_found", m),
            GithubError::Invalid(m) => Self::bad(m),
            GithubError::Exists(m) => Self::new("exists", m),
            GithubError::Revoked => Self::new("revoked", e.to_string()),
            GithubError::Unavailable(m) => {
                tracing::warn!(error = %m, "GitHub unavailable");
                Self::new(
                    "unavailable",
                    "GitHub or the database did not answer; retry later",
                )
            }
        }
    }
}

/// The GitHub queries of the settings page.
pub async fn run(s: &AppInner, who: &Identity, q: Query) -> Result<Value, ReadError> {
    let Some(github) = &s.github else {
        return match q {
            Query::Github => Ok(json!({ "configured": false, "account": null, "mirrors": [] })),
            _ => Err(ReadError::new(
                "not_configured",
                "GitHub is not configured on this server",
            )),
        };
    };
    match q {
        Query::Github => Ok(github.status(who).await?),
        Query::GithubRepos => Ok(json!({ "repos": github.repos(who).await? })),
        Query::GithubMirror { github: full_name } => {
            Ok(json!({ "mirror": github.mirror(who, &full_name).await? }))
        }
        Query::GithubUnmirror { name } => {
            github.unmirror(who, &name).await?;
            Ok(json!({}))
        }
        Query::GithubDisconnect => {
            github.disconnect(who).await?;
            Ok(json!({}))
        }
        _ => Err(ReadError::bad("not a GitHub query")),
    }
}
