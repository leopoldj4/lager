//! Personal access tokens of the signed-in user
//! (docs/decisions/0009-personal-access-tokens.md).
use super::{Web, json_error};
use crate::http::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use std::collections::BTreeMap;
use worktree_core::identity::{Identity, NewToken, Token, TokenId, UserError, validate_token};
use worktree_core::repo::Access;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/-/api/tokens", get(list).post(create))
        .route("/-/api/tokens/{id}", delete(remove))
}

/// The web interface and the signed-in user. `write`: the request changes
/// something, so it must come from this site.
async fn user<'a>(
    s: &'a AppState,
    headers: &HeaderMap,
    write: bool,
) -> Result<(&'a Web, String), Response> {
    signed_in(s, headers, write, "sign in to manage your access tokens").await
}

/// [`user`] with the message for anonymous requests.
pub(super) async fn signed_in<'a>(
    s: &'a AppState,
    headers: &HeaderMap,
    write: bool,
    anonymous: &str,
) -> Result<(&'a Web, String), Response> {
    let web = s
        .web
        .as_ref()
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    if write && !web.origin_allowed(headers) {
        s.registry.svc.metrics.inc("web_origin_rejections");
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "origin not allowed",
        ));
    }
    match web.identity(&s.directory, headers).await? {
        Identity::User { name, .. } => Ok((web, name)),
        Identity::Anonymous => Err(json_error(
            StatusCode::UNAUTHORIZED,
            "bad_credentials",
            anonymous,
        )),
    }
}

fn failure(error: UserError) -> Response {
    match error {
        UserError::Invalid(m) => json_error(StatusCode::BAD_REQUEST, "bad_request", &m),
        UserError::TokenNotFound(_) => {
            json_error(StatusCode::NOT_FOUND, "not_found", "token not found")
        }
        e @ UserError::SshKeyNotFound(_) => {
            json_error(StatusCode::NOT_FOUND, "not_found", &e.to_string())
        }
        UserError::NotFound(_) | UserError::Exists(_) => json_error(
            StatusCode::FORBIDDEN,
            "unsupported",
            "access tokens need a user in the database",
        ),
        UserError::Unavailable(m) => {
            tracing::warn!(error = %m, "token request unavailable");
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "access tokens unavailable; retry later",
            )
        }
    }
}

fn token_json(t: &Token) -> serde_json::Value {
    serde_json::json!({
        "id": t.id,
        "name": t.name,
        "access": t.access,
        "created_ms": t.created_ms,
        "expires_ms": t.expires_ms,
        "last_used_ms": t.last_used_ms,
    })
}

async fn list(State(s): State<AppState>, headers: HeaderMap) -> Result<Response, Response> {
    let (_, name) = user(&s, &headers, false).await?;
    let tokens = s.directory.users().tokens(&name).await.map_err(failure)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "tokens": tokens.iter().map(token_json).collect::<Vec<_>>(),
        })),
    )
        .into_response())
}

#[derive(serde::Deserialize)]
struct Create {
    name: String,
    access: Option<BTreeMap<String, Access>>,
    expires_days: Option<u32>,
}

async fn create(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Create>,
) -> Result<Response, Response> {
    let (_, name) = user(&s, &headers, true).await?;
    let new = NewToken {
        name: body.name,
        access: body.access,
        expires_days: body.expires_days,
    };
    validate_token(&new).map_err(failure)?;
    let (token, secret) = s
        .directory
        .users()
        .create_token(&name, new)
        .await
        .map_err(failure)?;
    tracing::info!(user = %name, token = token.id, "created an access token on the web");
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "token": token_json(&token), "secret": secret })),
    )
        .into_response())
}

async fn remove(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<TokenId>,
) -> Result<Response, Response> {
    let (_, name) = user(&s, &headers, true).await?;
    s.directory
        .users()
        .delete_token(&name, id)
        .await
        .map_err(failure)?;
    tracing::info!(user = %name, token = id, "deleted an access token on the web");
    Ok(StatusCode::NO_CONTENT.into_response())
}
