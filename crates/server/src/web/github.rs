//! Connecting a GitHub account, and the GitHub queries of the settings page
//! (docs/decisions/0020-github-mirrors.md).
//!
//! `GET /-/api/github/connect` sends the signed-in user to GitHub with a
//! random state in a short-lived cookie; GitHub sends them back to
//! `GET /-/api/github/callback`, which checks the state, keeps the token,
//! and returns to `/-/settings/github`. Both are page loads, not API calls.
//!
//! `GET /-/api/github/sign-in` signs in with GitHub instead, or links the
//! GitHub account of a signed-in user, through the same callback
//! (docs/decisions/0031-github-sign-in.md). A GitHub account that signs in
//! nobody leads to `/-/sign-up/github`, which picks a user name and creates
//! the account with `POST /-/api/github/sign-up`.
//!
//! `POST /-/api/github/hook/<repository UUID>` takes a mirror's webhook
//! deliveries (docs/decisions/0027-live-github-mirrors.md).

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query as UrlQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::ops::{Empty, Op, OpError};
use super::passkeys::{owns_repositories, refused, signed_in as session_response, taken};
use super::{Web, json_error};
use crate::github::{Delivery, Github, GithubError, GithubRepo, GithubStatus};
use crate::http::{AppInner, AppState};
use lager_core::github::Mirror;
use lager_core::identity::passkeys::PasskeyError;
use lager_core::identity::{Identity, validate_user_name};
use lager_core::limits;
use lager_core::model::RepoId;
use lager_core::password::constant_time_eq;

const COOKIE: &str = "lager_github_state";
/// The state of a sign-in, a dot, and where to go after it, in base64.
const SIGN_IN_COOKIE: &str = "lager_github_sign_in";
/// The GitHub account of a sign-up, sealed with the session secret.
const SIGN_UP_COOKIE: &str = "lager_github_sign_up";
const SIGN_UP_PURPOSE: &str = "github-sign-up";
const COOKIE_PATH: &str = "/-/api/github";
/// How long a trip to GitHub, and a sign-up after it, may take.
const COOKIE_SECS: u32 = 600;
const SETTINGS: &str = "/-/settings/github";
const SIGN_IN: &str = "/-/sign-in";
const SIGN_UP: &str = "/-/sign-up/github";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/-/api/github/connect", get(connect))
        .route("/-/api/github/sign-in", get(sign_in))
        .route("/-/api/github/sign-up", get(sign_up_pending).post(sign_up))
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
    redirect_with(to, cookie.into_iter().collect())
}

fn redirect_with(to: &str, cookies: Vec<String>) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    let h = r.headers_mut();
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(to).unwrap_or(HeaderValue::from_static(SETTINGS)),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for c in cookies
        .into_iter()
        .filter_map(|c| HeaderValue::from_str(&c).ok())
    {
        h.append(header::SET_COOKIE, c);
    }
    r
}

fn with_error(page: &str, message: &str, cookie: Option<String>) -> Response {
    redirect(
        &format!(
            "{page}?github_error={}",
            utf8_percent_encode(message, NON_ALPHANUMERIC)
        ),
        cookie,
    )
}

fn settings_error(message: &str, cookie: Option<String>) -> Response {
    with_error(SETTINGS, message, cookie)
}

fn state_cookie(s: &AppState, value: &str, max_age: u32) -> String {
    named_cookie(s, COOKIE, value, max_age)
}

fn named_cookie(s: &AppState, name: &str, value: &str, max_age: u32) -> String {
    let secure = s.web.as_ref().is_some_and(|w| w.sessions.secure());
    format!(
        "{name}={value}; Path={COOKIE_PATH}; Max-Age={max_age}; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

fn new_state() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// The callback URL on the public origin; without one, GitHub uses the
/// app's.
fn callback_url(s: &AppState) -> Option<String> {
    s.web
        .as_ref()
        .and_then(|w| w.public_origin.as_deref())
        .map(|o| format!("{}/-/api/github/callback", o.trim_end_matches('/')))
}

/// Where a sign-in may go afterwards: a page of this site, as the sign-in
/// page allows.
fn safe_next(next: Option<&str>) -> Option<&str> {
    next.filter(|n| {
        n.starts_with('/')
            && !n.starts_with("//")
            && !n.contains('\\')
            && (!n.starts_with("/-/") || *n == "/-/settings" || n.starts_with("/-/settings/"))
    })
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
    let state = new_state();
    tracing::debug!(user, "connecting GitHub");
    redirect(
        &github.authorize_url(&state, callback_url(&s).as_deref()),
        Some(state_cookie(&s, &state, COOKIE_SECS)),
    )
}

#[derive(serde::Deserialize)]
struct SignIn {
    next: Option<String>,
}

/// Sends the browser to GitHub to sign in, or, with a session, to link the
/// GitHub account for sign-in.
async fn sign_in(State(s): State<AppState>, UrlQuery(q): UrlQuery<SignIn>) -> Response {
    let Some(github) = &s.github else {
        return with_error(SIGN_IN, "GitHub is not configured on this server", None);
    };
    let state = new_state();
    let next = safe_next(q.next.as_deref()).unwrap_or_default();
    let value = format!("{state}.{}", base64::Engine::encode(&B64, next));
    redirect(
        &github.sign_in_url(&state, callback_url(&s).as_deref()),
        Some(named_cookie(&s, SIGN_IN_COOKIE, &value, COOKIE_SECS)),
    )
}

#[derive(serde::Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error_description: Option<String>,
}

/// Whether the callback's state is that of `cookie`; answers the rest of
/// the cookie's value after the state, if any.
fn state_of(headers: &HeaderMap, cookie: &str, q: &Callback) -> Option<String> {
    let value = super::session::cookie_value(headers, cookie)?;
    let (state, rest) = value.split_once('.').unwrap_or((&value, ""));
    let got = q.state.as_deref()?;
    constant_time_eq(state.as_bytes(), got.as_bytes()).then(|| rest.to_string())
}

/// GitHub sends the user back here.
async fn callback(
    State(s): State<AppState>,
    headers: HeaderMap,
    UrlQuery(q): UrlQuery<Callback>,
) -> Response {
    if let Some(next) = state_of(&headers, SIGN_IN_COOKIE, &q) {
        let next = String::from_utf8(base64::Engine::decode(&B64, next).unwrap_or_default())
            .unwrap_or_default();
        return signed_in_with_github(&s, &headers, q, safe_next(Some(&next))).await;
    }
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
    let state_ok = state_of(&headers, COOKIE, &q).is_some();
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

/// The callback of `GET /-/api/github/sign-in`, whose state matched.
async fn signed_in_with_github(
    s: &AppState,
    headers: &HeaderMap,
    q: Callback,
    next: Option<&str>,
) -> Response {
    let clear = named_cookie(s, SIGN_IN_COOKIE, "", 0);
    let (Some(web), Some(github)) = (&s.web, &s.github) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let current = match signed_in(s, headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let page = if current.is_some() { SETTINGS } else { SIGN_IN };
    if let Some(m) = q.error_description {
        return with_error(page, &m, Some(clear));
    }
    let Some(code) = q.code else {
        return with_error(page, "GitHub sent no code; try again", Some(clear));
    };
    let (id, login) = match github.finish_sign_in(&code).await {
        Ok(me) => me,
        Err(e) => {
            tracing::warn!(error = %e, "GitHub sign-in failed");
            return with_error(page, &e.to_string(), Some(clear));
        }
    };
    let signs_in = match github.db.signs_in(id, &login).await {
        Ok(u) => u,
        Err(e) => return with_error(page, &GithubError::from(e).to_string(), Some(clear)),
    };
    if let Some(user) = current {
        return link(github, &user, id, &login, signs_in, next, clear).await;
    }
    let Some(user) = signs_in else {
        if !web.sign_up {
            s.registry.svc.metrics.inc("web_sign_in_failures");
            let m = format!("no account here uses @{login}");
            return with_error(SIGN_IN, &m, Some(clear));
        }
        let pending = web.sessions.seal(
            SIGN_UP_PURPOSE,
            &format!("{id}:{login}"),
            COOKIE_SECS.into(),
        );
        return redirect_with(
            SIGN_UP,
            vec![
                clear,
                named_cookie(s, SIGN_UP_COOKIE, &pending, COOKIE_SECS),
            ],
        );
    };
    let found = match s.directory.lookup(&user).await {
        Ok(Some(found)) => found,
        Ok(None) => return with_error(SIGN_IN, "the account is gone", Some(clear)),
        Err(_) => {
            return with_error(SIGN_IN, "user lookup unavailable; retry later", Some(clear));
        }
    };
    s.registry.svc.metrics.inc("web_sign_ins");
    tracing::info!(user = %user, github = %login, "signed in on the web with GitHub");
    redirect_with(
        next.unwrap_or("/"),
        vec![
            clear,
            web.sessions.issue(&user, &found.stamp),
            web.sessions.clear_old(),
        ],
    )
}

/// Links the GitHub account for sign-in to the signed-in `user`.
async fn link(
    github: &Github,
    user: &str,
    id: i64,
    login: &str,
    signs_in: Option<String>,
    next: Option<&str>,
    clear: String,
) -> Response {
    let refuse = |m: String| settings_error(&m, Some(clear.clone()));
    match signs_in {
        Some(other) if other != user => {
            return refuse(format!("@{login} already signs in another user"));
        }
        Some(_) => {}
        None => match github.db.link(user, id, login).await {
            Ok(true) => tracing::info!(user, github = %login, "GitHub sign-in linked"),
            Ok(false) => {
                return refuse("unlink the GitHub account that signs you in first".into());
            }
            Err(e) => return refuse(GithubError::from(e).to_string()),
        },
    }
    redirect(next.unwrap_or(SETTINGS), Some(clear))
}

/// The GitHub account of the sign-up that the cookie holds.
fn pending(web: &Web, headers: &HeaderMap) -> Option<(i64, String)> {
    let sealed = super::session::cookie_value(headers, SIGN_UP_COOKIE)?;
    let value = web.sessions.open(SIGN_UP_PURPOSE, &sealed)?;
    let (id, login) = value.split_once(':')?;
    Some((id.parse().ok()?, login.to_string()))
}

fn expired() -> Response {
    json_error(
        StatusCode::GONE,
        "expired",
        "the GitHub sign-in expired; sign in with GitHub again",
    )
}

/// The GitHub login of a pending sign-up, for the page that picks a name.
async fn sign_up_pending(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let Some(web) = s.web.as_ref().filter(|w| w.sign_up && s.github.is_some()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match pending(web, &headers) {
        Some((_, login)) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "login": login })),
        )
            .into_response(),
        None => expired(),
    }
}

#[derive(serde::Deserialize)]
struct SignUp {
    user: String,
}

/// Creates the account of a pending sign-up and signs it in.
async fn sign_up(State(s): State<AppState>, headers: HeaderMap, body: Json<SignUp>) -> Response {
    let Some(web) = s.web.as_ref().filter(|_| s.github.is_some()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !web.sign_up {
        return json_error(
            StatusCode::NOT_FOUND,
            "disabled",
            "sign-up is not enabled on this server",
        );
    }
    if !web.origin_allowed(&headers) {
        s.registry.svc.metrics.inc("web_origin_rejections");
        return json_error(StatusCode::FORBIDDEN, "forbidden", "origin not allowed");
    }
    let Some((id, login)) = pending(web, &headers) else {
        return expired();
    };
    let user = body.0.user;
    if let Err(e) = validate_user_name(&user) {
        return json_error(StatusCode::BAD_REQUEST, "bad_request", &e.to_string());
    }
    // Repositories under a name belong to whoever has it (decision 18).
    match owns_repositories(&s, &user).await {
        Ok(false) => {}
        Ok(true) => return taken(&user),
        Err(response) => return response,
    }
    match s
        .directory
        .users()
        .create_signed_up_with_github(&user, id, &login)
        .await
    {
        Ok(stamp) => {
            s.registry.svc.metrics.inc("web_sign_ups");
            tracing::info!(user = %user, github = %login, "signed up on the web with GitHub");
            let mut r = session_response(web, &user, &stamp);
            if let Ok(c) = HeaderValue::from_str(&named_cookie(&s, SIGN_UP_COOKIE, "", 0)) {
                r.headers_mut().append(header::SET_COOKIE, c);
            }
            r
        }
        Err(e) => refused(e),
    }
}

impl From<GithubError> for OpError {
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

// The GitHub queries and changes of the settings page
// (docs/decisions/0020-github-mirrors.md). They need a session.

fn configured(s: &AppInner) -> Result<&Github, OpError> {
    s.github
        .as_deref()
        .ok_or_else(|| OpError::new("not_configured", "GitHub is not configured on this server"))
}

#[derive(Serialize, TS)]
pub struct GithubRepoList {
    repos: Vec<GithubRepo>,
}

#[derive(Serialize, TS)]
pub struct MirrorCreated {
    mirror: Mirror,
}

/// The user's GitHub account and mirrors.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubOp {}

impl Op for GithubOp {
    type Answer = GithubStatus;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<GithubStatus, OpError> {
        match &s.github {
            Some(github) => Ok(github.status(who).await?),
            None => Ok(GithubStatus::default()),
        }
    }
}

/// The GitHub repositories the user can mirror.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubReposOp {}

impl Op for GithubReposOp {
    type Answer = GithubRepoList;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<GithubRepoList, OpError> {
        let repos = configured(s)?.repos(who).await?;
        Ok(GithubRepoList { repos })
    }
}

/// Mirrors a GitHub repository.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubMirrorOp {
    /// `owner/name`.
    github: String,
}

impl Op for GithubMirrorOp {
    type Answer = MirrorCreated;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<MirrorCreated, OpError> {
        let mirror = configured(s)?.mirror(who, &self.github).await?;
        Ok(MirrorCreated { mirror })
    }
}

/// Stops syncing the user's mirror `name`; it takes pushes again.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubUnmirrorOp {
    name: String,
}

impl Op for GithubUnmirrorOp {
    type Answer = Empty;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Empty, OpError> {
        configured(s)?.unmirror(who, &self.name).await?;
        Ok(Empty {})
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubDisconnectOp {}

impl Op for GithubDisconnectOp {
    type Answer = Empty;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Empty, OpError> {
        configured(s)?.disconnect(who).await?;
        Ok(Empty {})
    }
}

/// Turns syncing new GitHub repositories on or off.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubAutoOp {
    on: bool,
}

impl Op for GithubAutoOp {
    type Answer = Empty;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Empty, OpError> {
        configured(s)?.set_auto(who, self.on).await?;
        Ok(Empty {})
    }
}

/// Stops the user's GitHub account from signing in
/// (docs/decisions/0031-github-sign-in.md).
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubUnlinkOp {}

impl Op for GithubUnlinkOp {
    type Answer = Empty;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Empty, OpError> {
        configured(s)?;
        let Identity::User { name, .. } = who else {
            return Err(GithubError::SignedOut.into());
        };
        match s.directory.users().unlink_github(name).await {
            Ok(()) => Ok(Empty {}),
            Err(PasskeyError::Conflict(m)) => Err(OpError::new("conflict", m)),
            Err(PasskeyError::Invalid(m)) => Err(OpError::bad(m)),
            Err(PasskeyError::Auth(_)) => Err(OpError::new(
                "unavailable",
                "the database did not answer; retry later",
            )),
        }
    }
}
