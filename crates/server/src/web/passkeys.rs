//! Browser passkey endpoints. Origin and browser binding are required on every ceremony.
use super::{Web, json_error};
use crate::http::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};
use worktree_core::identity::{AuthError, Identity};

const COOKIE: &str = "worktree_passkey";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/-/api/passkeys", get(list))
        .route("/-/api/passkeys/{id}", delete(remove))
        .route("/-/api/passkeys/register/start", post(register_start))
        .route("/-/api/passkeys/register/finish", post(register_finish))
        .route("/-/api/passkeys/login/start", post(login_start))
        .route("/-/api/passkeys/login/finish", post(login_finish))
}

fn context<'a>(s: &'a AppState, headers: &HeaderMap) -> Result<(&'a Web, &'a str), Response> {
    let web = s
        .web
        .as_ref()
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    let origin = web.passkey_origin.as_deref().ok_or_else(|| {
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
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "enabled": web.passkey_origin.is_some(), "passkeys": keys })),
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
        .map_err(failure)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(serde::Deserialize)]
struct Registration {
    password: String,
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
            .start_passkey(origin, &name, Some((&body.password, &body.name)))
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
#[derive(serde::Deserialize)]
struct Login {
    user: String,
}
async fn login_start(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Result<Response, Response> {
    let (_, origin) = context(&s, &headers)?;
    Ok(started(
        s.directory
            .users()
            .start_passkey(origin, &body.user, None)
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
            let mut response = Json(serde_json::json!({ "user": user })).into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&web.sessions.issue(&user, &stamp)).expect("session cookie"),
            );
            response.headers_mut().append(
                header::SET_COOKIE,
                HeaderValue::from_str(&web.sessions.clear_old()).expect("session cookie"),
            );
            response
        }
        Err(e) => {
            s.registry.svc.metrics.inc("web_sign_in_failures");
            failure(e)
        }
    };
    Ok(finished(response, origin))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ceremony_cookie_is_bound_to_browser_and_secure_origin() {
        let value = cookie("https://git.example.com", &"a".repeat(43), 300);
        assert!(
            value
                .to_str()
                .unwrap()
                .contains("HttpOnly; SameSite=Strict; Max-Age=300; Secure")
        );
        assert!(
            !cookie("http://localhost:3000", "", 0)
                .to_str()
                .unwrap()
                .contains("Secure")
        );
        let mut headers = HeaderMap::new();
        assert!(binding(&headers).is_err());
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("other=value; {COOKIE}={}", "a".repeat(43))).unwrap(),
        );
        assert_eq!(binding(&headers).unwrap(), "a".repeat(43));
    }
}
