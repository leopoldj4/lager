//! Email addresses of the signed-in user: commits with one of them as the
//! author email count on the user's profile
//! (docs/decisions/0029-activity-graph.md). A connected GitHub account
//! adds the addresses it verified; nothing checks the ones added here, and
//! a profile counts only repositories its user can read.
use super::json_error;
use super::tokens::signed_in;
use crate::http::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use lager_core::identity::{UserError, normalize_email};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/-/api/emails", get(list).post(add))
        .route("/-/api/emails/{email}", delete(remove))
}

const ANONYMOUS: &str = "sign in to manage your email addresses";

fn failure(error: UserError) -> Response {
    match error {
        UserError::Invalid(m) => json_error(StatusCode::BAD_REQUEST, "bad_request", &m),
        UserError::NotFound(_)
        | UserError::Exists(_)
        | UserError::TokenNotFound(_)
        | UserError::SshKeyNotFound(_) => json_error(
            StatusCode::FORBIDDEN,
            "unsupported",
            "email addresses need a user in the database",
        ),
        UserError::Unavailable(m) => {
            tracing::warn!(error = %m, "email request unavailable");
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "email addresses unavailable; retry later",
            )
        }
    }
}

async fn list(State(s): State<AppState>, headers: HeaderMap) -> Result<Response, Response> {
    let (_, user) = signed_in(&s, &headers, false, ANONYMOUS).await?;
    let emails = s.directory.users().emails(&user).await.map_err(failure)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "emails": emails
                .iter()
                .map(|(email, github)| serde_json::json!({ "email": email, "github": github }))
                .collect::<Vec<_>>(),
        })),
    )
        .into_response())
}

#[derive(serde::Deserialize)]
struct Add {
    email: String,
}

async fn add(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Add>,
) -> Result<Response, Response> {
    let (_, user) = signed_in(&s, &headers, true, ANONYMOUS).await?;
    let email = normalize_email(&body.email).map_err(failure)?;
    s.directory
        .users()
        .add_email(&user, &email)
        .await
        .map_err(failure)?;
    tracing::info!(user = %user, "added an email address on the web");
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "email": email })),
    )
        .into_response())
}

async fn remove(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(email): Path<String>,
) -> Result<Response, Response> {
    let (_, user) = signed_in(&s, &headers, true, ANONYMOUS).await?;
    let email = email.trim().to_lowercase();
    if !s
        .directory
        .users()
        .delete_email(&user, &email)
        .await
        .map_err(failure)?
    {
        return Err(json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "email address not found",
        ));
    }
    tracing::info!(user = %user, "deleted an email address on the web");
    Ok(StatusCode::NO_CONTENT.into_response())
}
