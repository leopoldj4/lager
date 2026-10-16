//! SSH keys of the signed-in user, for Git over SSH
//! (docs/decisions/0022-git-over-ssh.md). A key has its user's access.
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
use worktree_core::identity::{
    SshKey, SshKeyId, UserError, parse_public_key, validate_ssh_key_name,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/-/api/ssh-keys", get(list).post(add))
        .route("/-/api/ssh-keys/{id}", delete(remove))
}

const ANONYMOUS: &str = "sign in to manage your SSH keys";

fn failure(error: UserError) -> Response {
    match error {
        UserError::Invalid(m) => json_error(StatusCode::BAD_REQUEST, "bad_request", &m),
        UserError::SshKeyNotFound(_) | UserError::TokenNotFound(_) => {
            json_error(StatusCode::NOT_FOUND, "not_found", "SSH key not found")
        }
        UserError::NotFound(_) | UserError::Exists(_) => json_error(
            StatusCode::FORBIDDEN,
            "unsupported",
            "SSH keys need a user in the database",
        ),
        UserError::Unavailable(m) => {
            tracing::warn!(error = %m, "SSH key request unavailable");
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "SSH keys unavailable; retry later",
            )
        }
    }
}

fn key_json(k: &SshKey) -> serde_json::Value {
    serde_json::json!({
        "id": k.id,
        "name": k.name,
        "fingerprint": k.fingerprint,
        "algorithm": k.public_key.split_once(' ').map_or("", |(a, _)| a),
        "created_ms": k.created_ms,
        "last_used_ms": k.last_used_ms,
    })
}

async fn list(State(s): State<AppState>, headers: HeaderMap) -> Result<Response, Response> {
    let (_, name) = signed_in(&s, &headers, false, ANONYMOUS).await?;
    let keys = s.directory.users().ssh_keys(&name).await.map_err(failure)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "keys": keys.iter().map(key_json).collect::<Vec<_>>(),
        })),
    )
        .into_response())
}

#[derive(serde::Deserialize)]
struct Add {
    /// A public key line, such as the contents of `~/.ssh/id_ed25519.pub`.
    key: String,
    /// Without it, the key's comment.
    name: Option<String>,
}

async fn add(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Add>,
) -> Result<Response, Response> {
    let (_, user) = signed_in(&s, &headers, true, ANONYMOUS).await?;
    let key = parse_public_key(&body.key).map_err(failure)?;
    let name = body
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| key.comment.clone())
        .unwrap_or_else(|| key.algorithm.clone());
    validate_ssh_key_name(&name).map_err(failure)?;
    let added = s
        .directory
        .users()
        .add_ssh_key(&user, &name, &key)
        .await
        .map_err(failure)?;
    tracing::info!(user = %user, ssh_key = added.id, fingerprint = %added.fingerprint, "added an SSH key on the web");
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "key": key_json(&added) })),
    )
        .into_response())
}

async fn remove(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<SshKeyId>,
) -> Result<Response, Response> {
    let (_, user) = signed_in(&s, &headers, true, ANONYMOUS).await?;
    s.directory
        .users()
        .delete_ssh_key(&user, id)
        .await
        .map_err(failure)?;
    tracing::info!(user = %user, ssh_key = id, "deleted an SSH key on the web");
    Ok(StatusCode::NO_CONTENT.into_response())
}
