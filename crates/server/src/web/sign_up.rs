//! Sign-up with a user name and password (docs/decisions/0025-password-sign-up.md).
//! With `LAGER_SIGN_UP`, `GET /-/api/signup` tells the pages which ways are
//! offered; passkey sign-up is under `/-/api/passkeys/signup/`.
use super::json_error;
use super::passkeys::{owns_repositories, refused, signed_in, taken};
use crate::http::AppState;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use lager_core::identity::{validate_password, validate_user_name};
use lager_core::password::{DEFAULT_ITERATIONS, PasswordHash};

pub fn router() -> Router<AppState> {
    Router::new().route("/-/api/signup", get(offered).post(sign_up))
}

/// Which ways of signing up the sign-in page offers.
async fn offered(State(s): State<AppState>) -> Response {
    let web = s.web.as_ref().filter(|w| w.sign_up);
    Json(serde_json::json!({
        "enabled": web.is_some(),
        "passkey": web.is_some_and(|w| w.public_origin.is_some()),
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct SignUp {
    user: String,
    password: String,
}

async fn sign_up(State(s): State<AppState>, headers: HeaderMap, body: Json<SignUp>) -> Response {
    let Some(web) = s.web.as_ref() else {
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
    let SignUp { user, password } = body.0;
    if let Err(e) = validate_user_name(&user).and_then(|()| validate_password(&password)) {
        return json_error(StatusCode::BAD_REQUEST, "bad_request", &e.to_string());
    }
    // Repositories under a name belong to whoever has it (decision 18).
    match owns_repositories(&s, &user).await {
        Ok(false) => {}
        Ok(true) => return taken(&user),
        Err(response) => return response,
    }
    let hash = match tokio::task::spawn_blocking(move || {
        PasswordHash::new(&password, DEFAULT_ITERATIONS).to_string()
    })
    .await
    {
        Ok(hash) => hash,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match s
        .directory
        .users()
        .create_signed_up_with_password(&user, &hash)
        .await
    {
        Ok(stamp) => {
            s.registry.svc.metrics.inc("web_sign_ups");
            tracing::info!(user = %user, "signed up on the web with a password");
            signed_in(web, &user, &stamp)
        }
        Err(e) => refused(e),
    }
}
