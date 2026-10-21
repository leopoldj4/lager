//! Browser passkey endpoints. Origin and browser binding are required on every ceremony.
//! With `LAGER_SIGN_UP`, `signup/` creates accounts whose only credential is a passkey.
use super::{Web, json_error};
use crate::http::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use lager_core::identity::passkeys::PasskeyError;
use lager_core::identity::{AuthError, Identity};
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

const COOKIE: &str = "lager_passkey";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/-/api/passkeys", get(list))
        .route("/-/api/passkeys/{id}", delete(remove))
        .route("/-/api/passkeys/register/start", post(register_start))
        .route("/-/api/passkeys/register/finish", post(register_finish))
        .route("/-/api/passkeys/login/start", post(login_start))
        .route("/-/api/passkeys/login/finish", post(login_finish))
        .route("/-/api/passkeys/signup/start", post(sign_up_start))
        .route("/-/api/passkeys/signup/finish", post(sign_up_finish))
}

fn context<'a>(s: &'a AppState, headers: &HeaderMap) -> Result<(&'a Web, &'a str), Response> {
    let web = s
        .web
        .as_ref()
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    let origin = web.public_origin.as_deref().ok_or_else(|| {
        json_error(
            StatusCode::NOT_FOUND,
            "disabled",
            "passkeys are not configured on this server",
        )
    })?;
    if headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) != Some(origin) {
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "origin not allowed",
        ));
    }
    Ok((web, origin))
}

async fn user(s: &AppState, web: &Web, headers: &HeaderMap) -> Result<String, Response> {
    match web.identity(&s.directory, headers).await? {
        Identity::User { name, .. } => Ok(name),
        Identity::Anonymous => Err(failure(AuthError::BadCredentials)),
    }
}

fn failure(error: AuthError) -> Response {
    match error {
        AuthError::BadCredentials => json_error(
            StatusCode::UNAUTHORIZED,
            "bad_credentials",
            "passkey request failed; check your credentials and try again",
        ),
        AuthError::Unavailable(message) => {
            tracing::warn!(error = %message, "passkey request unavailable");
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "passkeys unavailable; retry later",
            )
        }
    }
}

pub(super) fn refused(error: PasskeyError) -> Response {
    match error {
        PasskeyError::Invalid(m) => json_error(StatusCode::BAD_REQUEST, "bad_request", &m),
        PasskeyError::Conflict(m) => json_error(StatusCode::CONFLICT, "conflict", &m),
        PasskeyError::Auth(e) => failure(e),
    }
}

fn cookie(origin: &str, id: &str, age: u16) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={id}; Path=/-/api/passkeys; HttpOnly; SameSite=Strict; Max-Age={age}{}",
        if origin.starts_with("https://") {
            "; Secure"
        } else {
            ""
        }
    ))
    .expect("generated cookie")
}
fn binding(headers: &HeaderMap) -> Result<&str, Response> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|v| v.trim().strip_prefix(&format!("{COOKIE}=")))
        .filter(|v| v.len() == 43)
        .ok_or_else(|| failure(AuthError::BadCredentials))
}
fn started(result: Result<(String, serde_json::Value), AuthError>, origin: &str) -> Response {
    match result {
        Ok((id, options)) => {
            let mut response = Json(options).into_response();
            response
                .headers_mut()
                .insert(header::SET_COOKIE, cookie(origin, &id, 300));
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(e) => failure(e),
    }
}
fn finished(mut response: Response, origin: &str) -> Response {
    response
        .headers_mut()
        .append(header::SET_COOKIE, cookie(origin, "", 0));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

// Same-origin GET does not always carry Origin; no cross-origin reading is enabled.
async fn list(State(s): State<AppState>, headers: HeaderMap) -> Result<Response, Response> {
    let web = s
        .web
        .as_ref()
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    let name = user(&s, web, &headers).await?;
    let keys = s.directory.users().passkeys(&name).await.map_err(failure)?;
    let password = s
        .directory
        .users()
        .has_password(&name)
        .await
        .map_err(failure)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "enabled": web.public_origin.is_some(),
            "password": password,
            "passkeys": keys,
        })),
    )
        .into_response())
}
async fn remove(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let (web, _) = context(&s, &headers)?;
    let name = user(&s, web, &headers).await?;
    s.directory
        .users()
        .delete_passkey(&name, &id)
        .await
        .map_err(refused)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(serde::Deserialize)]
struct Registration {
    name: String,
}
async fn register_start(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Registration>,
) -> Result<Response, Response> {
    let (web, origin) = context(&s, &headers)?;
    let name = user(&s, web, &headers).await?;
    Ok(started(
        s.directory
            .users()
            .start_passkey_registration(origin, &name, &body.name)
            .await,
        origin,
    ))
}
async fn register_finish(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterPublicKeyCredential>,
) -> Result<Response, Response> {
    let (web, origin) = context(&s, &headers)?;
    let name = user(&s, web, &headers).await?;
    let id = binding(&headers)?;
    let response = match s
        .directory
        .users()
        .finish_passkey_registration(origin, id, &name, &body)
        .await
    {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => failure(e),
    };
    Ok(finished(response, origin))
}
async fn login_start(State(s): State<AppState>, headers: HeaderMap) -> Result<Response, Response> {
    let (_, origin) = context(&s, &headers)?;
    Ok(started(
        s.directory
            .users()
            .start_passkey_authentication(origin, binding(&headers).ok())
            .await,
        origin,
    ))
}
async fn login_finish(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PublicKeyCredential>,
) -> Result<Response, Response> {
    let (web, origin) = context(&s, &headers)?;
    let id = binding(&headers)?;
    let response = match s
        .directory
        .users()
        .finish_passkey_authentication(origin, id, &body)
        .await
    {
        Ok((user, stamp)) => {
            s.registry.svc.metrics.inc("web_sign_ins");
            signed_in(web, &user, &stamp)
        }
        Err(e) => {
            s.registry.svc.metrics.inc("web_sign_in_failures");
            failure(e)
        }
    };
    Ok(finished(response, origin))
}

pub(super) fn signed_in(web: &Web, user: &str, stamp: &[u8; 32]) -> Response {
    let mut response = Json(serde_json::json!({ "user": user })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&web.sessions.issue(user, stamp)).expect("session cookie"),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&web.sessions.clear_old()).expect("session cookie"),
    );
    response
}

fn sign_up_context<'a>(
    s: &'a AppState,
    headers: &HeaderMap,
) -> Result<(&'a Web, &'a str), Response> {
    let (web, origin) = context(s, headers)?;
    if !web.sign_up {
        return Err(json_error(
            StatusCode::NOT_FOUND,
            "disabled",
            "sign-up is not enabled on this server",
        ));
    }
    Ok((web, origin))
}

#[derive(serde::Deserialize)]
struct SignUp {
    user: String,
}
async fn sign_up_start(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SignUp>,
) -> Result<Response, Response> {
    let (_, origin) = sign_up_context(&s, &headers)?;
    // Checked before the browser saves a passkey, and again before the account exists.
    if owns_repositories(&s, &body.user).await? {
        return Err(taken(&body.user));
    }
    let started_up = s
        .directory
        .users()
        .start_sign_up(origin, &body.user, binding(&headers).ok())
        .await
        .map_err(refused)?;
    Ok(started(Ok(started_up), origin))
}
async fn sign_up_finish(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterPublicKeyCredential>,
) -> Result<Response, Response> {
    let (web, origin) = sign_up_context(&s, &headers)?;
    let id = binding(&headers)?;
    let users = s.directory.users();
    let response = match users.finish_sign_up(origin, id, &body).await {
        Ok(sign_up) => match owns_repositories(&s, sign_up.name()).await {
            Err(response) => response,
            // Repositories under a name belong to whoever has it (decision 18).
            Ok(true) => taken(sign_up.name()),
            Ok(false) => {
                let user = sign_up.name().to_string();
                match users.create_signed_up(sign_up).await {
                    Ok(stamp) => {
                        s.registry.svc.metrics.inc("web_sign_ups");
                        tracing::info!(user = %user, "signed up on the web");
                        signed_in(web, &user, &stamp)
                    }
                    Err(e) => refused(e),
                }
            }
        },
        Err(e) => failure(e),
    };
    Ok(finished(response, origin))
}

pub(super) fn taken(user: &str) -> Response {
    json_error(
        StatusCode::CONFLICT,
        "conflict",
        &format!("the user name {user} is taken"),
    )
}

/// Whether any repository, even a deleted one, is named under `user`.
pub(super) async fn owns_repositories(s: &AppState, user: &str) -> Result<bool, Response> {
    let prefix = format!("{user}/").to_lowercase();
    match s.directory.catalog().list().await {
        Ok(records) => Ok(records
            .iter()
            .any(|r| r.name.to_lowercase().starts_with(&prefix))),
        Err(e) => {
            tracing::warn!(error = %e, "repository catalog unavailable");
            Err(json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "repository catalog unavailable; retry later",
            ))
        }
    }
}
