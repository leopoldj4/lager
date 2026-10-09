//! Admin API: JSON under `/_rustgit/api/` for user and repository management
//! (docs/decisions/0004-users-in-storage.md, 0006, 0008).
//!
//! `me` answers every authenticated user; everything else needs admin access
//! to every repository (`"*" = "admin"`). Users can be changed only when the
//! identity provider manages them (`[database]`), and not those from the
//! configuration file; repositories can be changed only with the storage
//! catalog.

use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};

use super::access::{AuthError, Identity};
use super::password::{DEFAULT_ITERATIONS, PasswordHash};
use super::{AppInner, AppState};
use crate::catalog::{Catalog, CatalogError};
use crate::config::{Access, RepoConfig};
use crate::identity::{self, UserChange, UserError};
use crate::model::{RepoId, RepoState};
use crate::repository::admin::AdminError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/repos", get(repos).post(create_repo))
        // Repository names can contain `/`.
        .route(
            "/repos/{*name}",
            axum::routing::patch(update_repo).delete(delete_repo),
        )
        .route("/users", get(users).post(create_user))
        .route(
            "/users/{name}",
            axum::routing::patch(update_user).delete(delete_user),
        )
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    let mut r = (status, Json(serde_json::json!({ "error": msg.into() }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Basic realm=\"rustgit\""),
        );
    }
    r
}

fn storage_error(e: impl std::fmt::Display) -> Response {
    tracing::warn!(error = %e, "storage unavailable");
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "storage unavailable; retry later",
    )
}

async fn identify(s: &AppInner, headers: &HeaderMap) -> Result<Identity, Response> {
    if !s.admin_api {
        return Err(error(StatusCode::NOT_FOUND, "not found"));
    }
    s.directory.reload_if_changed();
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    match s.directory.authenticate(auth).await {
        Ok(Identity::Anonymous) | Err(AuthError::BadCredentials) => {
            Err(error(StatusCode::UNAUTHORIZED, "authentication required"))
        }
        Ok(i) => Ok(i),
        Err(AuthError::Unavailable(m)) => {
            tracing::warn!(error = %m, "user lookup unavailable");
            Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "user lookup unavailable; retry later",
            ))
        }
    }
}

async fn admin(s: &AppInner, headers: &HeaderMap) -> Result<Identity, Response> {
    let who = identify(s, headers).await?;
    if who.is_global_admin() {
        Ok(who)
    } else {
        Err(error(
            StatusCode::FORBIDDEN,
            "admin access to every repository is required",
        ))
    }
}

async fn me(State(s): State<AppState>, headers: HeaderMap) -> Response {
    match identify(&s, &headers).await {
        Ok(who) => Json(serde_json::json!({
            "name": who.name(),
            "admin": who.is_global_admin(),
        }))
        .into_response(),
        Err(r) => r,
    }
}

#[derive(Serialize)]
struct RepoView {
    name: String,
    id: String,
    default_branch: String,
    anonymous_read: bool,
    customer: Option<String>,
    ref_rules: usize,
    /// Accepted changes so far; `None` if the head is missing.
    generation: Option<u64>,
    source: &'static str,
}

async fn repos(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin(&s, &headers).await {
        return r;
    }
    let (list, source): (Vec<RepoConfig>, _) = match s.directory.catalog() {
        Some(c) => match c.list().await {
            Ok(records) => (
                records
                    .into_iter()
                    .filter(|r| r.state == RepoState::Active)
                    .map(|r| r.repo_config())
                    .collect(),
                "catalog",
            ),
            Err(e) => return storage_error(e),
        },
        None => {
            let mut repos = s.directory.configured_repos();
            repos.sort_by(|a, b| a.name.cmp(&b.name));
            (repos, "config")
        }
    };
    let mut out = Vec::with_capacity(list.len());
    for r in list {
        match repo_view(&s, r, source).await {
            Ok(v) => out.push(v),
            Err(r) => return r,
        }
    }
    Json(out).into_response()
}

async fn repo_view(
    s: &AppInner,
    r: RepoConfig,
    source: &'static str,
) -> Result<RepoView, Response> {
    let generation = match s.registry.svc.storage.load_head(r.id).await {
        Ok(h) => h.map(|(h, _)| h.generation),
        Err(e) => return Err(storage_error(e)),
    };
    Ok(RepoView {
        name: r.name,
        id: r.id.to_string(),
        default_branch: r.default_branch,
        anonymous_read: r.anonymous_read,
        customer: r.customer,
        ref_rules: r.ref_rules.len(),
        generation,
        source,
    })
}

fn catalog_error(e: CatalogError) -> Response {
    match e {
        CatalogError::Exists(_) | CatalogError::Admin(AdminError::Exists) => {
            error(StatusCode::CONFLICT, e.to_string())
        }
        CatalogError::NotFound(_)
        | CatalogError::Admin(AdminError::NotFound | AdminError::Deleted) => {
            error(StatusCode::NOT_FOUND, e.to_string())
        }
        CatalogError::Invalid(_) => error(StatusCode::BAD_REQUEST, e.to_string()),
        CatalogError::Conflict | CatalogError::Admin(AdminError::Conflict) => {
            error(StatusCode::CONFLICT, e.to_string())
        }
        CatalogError::Unknown(m) | CatalogError::Admin(AdminError::Unknown(m)) => {
            tracing::warn!(error = %m, "repository change result unknown");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "result unknown; reload before repeating the change",
            )
        }
        CatalogError::Storage(e) | CatalogError::Admin(AdminError::Storage(e)) => storage_error(e),
    }
}

/// The caller and the catalog for a repository change, or the reason there
/// is none.
async fn manage_repos(
    s: &AppInner,
    headers: &HeaderMap,
) -> Result<(Identity, std::sync::Arc<Catalog>), Response> {
    let who = admin(s, headers).await?;
    match s.directory.catalog() {
        Some(c) => Ok((who, c.clone())),
        None => Err(error(
            StatusCode::CONFLICT,
            "repositories come from the configuration file; \
             use catalog.kind = \"storage\" to manage them here",
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewRepo {
    name: String,
    #[serde(default)]
    default_branch: Option<String>,
}

async fn create_repo(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(new): Json<NewRepo>,
) -> Response {
    let (who, catalog) = match manage_repos(&s, &headers).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut settings = RepoConfig::new(&new.name, RepoId::new());
    if let Some(b) = new.default_branch {
        settings.default_branch = b;
    }
    match catalog.create(&settings, who.name()).await {
        Ok(r) => {
            tracing::info!(repo = %r.name, id = %r.id, by = %who.name(), "created repository");
            match repo_view(&s, r, "catalog").await {
                Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
                Err(r) => r,
            }
        }
        Err(e) => catalog_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoChange {
    default_branch: String,
}

async fn update_repo(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(change): Json<RepoChange>,
) -> Response {
    let (who, catalog) = match manage_repos(&s, &headers).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match catalog
        .set_default_branch(&name, &change.default_branch, who.name())
        .await
    {
        Ok((r, _)) => {
            tracing::info!(repo = %name, branch = %change.default_branch, by = %who.name(), "changed default branch");
            match repo_view(&s, r, "catalog").await {
                Ok(v) => Json(v).into_response(),
                Err(r) => r,
            }
        }
        Err(e) => catalog_error(e),
    }
}

async fn delete_repo(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let (who, catalog) = match manage_repos(&s, &headers).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match catalog.delete(&name, who.name()).await {
        Ok(id) => {
            tracing::info!(repo = %name, %id, by = %who.name(), "deleted repository");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => catalog_error(e),
    }
}

async fn users(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin(&s, &headers).await {
        return r;
    }
    let ids = s.directory.identities();
    match ids.list().await {
        Ok(users) => Json(serde_json::json!({ "managed": ids.manages_users(), "users": users }))
            .into_response(),
        Err(e) => user_error(e),
    }
}

fn user_error(e: UserError) -> Response {
    match e {
        UserError::Exists(_) | UserError::Unmanaged(_) => {
            error(StatusCode::CONFLICT, e.to_string())
        }
        UserError::NotFound(_) => error(StatusCode::NOT_FOUND, e.to_string()),
        UserError::Invalid(_) => error(StatusCode::BAD_REQUEST, e.to_string()),
        UserError::Unavailable(m) => {
            tracing::warn!(error = %m, "user store unavailable");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "user store unavailable; retry later",
            )
        }
    }
}

/// PBKDF2 with the default iterations takes a noticeable time; it runs off
/// the request threads.
async fn hash(password: String) -> Result<String, Response> {
    identity::validate_password(&password).map_err(user_error)?;
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&password, DEFAULT_ITERATIONS).to_string()
    })
    .await
    .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
}

/// The caller for a user change, after the checks that need no hashing.
async fn manage_users(s: &AppInner, headers: &HeaderMap) -> Result<Identity, Response> {
    let who = admin(s, headers).await?;
    if !s.directory.identities().manages_users() {
        return Err(user_error(identity::unmanaged()));
    }
    Ok(who)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewUser {
    name: String,
    password: String,
    #[serde(default)]
    access: BTreeMap<String, Access>,
}

async fn create_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(new): Json<NewUser>,
) -> Response {
    let who = match manage_users(&s, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    if let Err(e) =
        identity::validate_user_name(&new.name).and_then(|_| identity::validate_access(&new.access))
    {
        return user_error(e);
    }
    let password_hash = match hash(new.password).await {
        Ok(h) => h,
        Err(r) => return r,
    };
    let new = identity::NewUser {
        name: new.name,
        password_hash,
        access: new.access,
    };
    match s.directory.identities().create(new, who.name()).await {
        Ok(u) => {
            tracing::info!(user = %u.name, by = %who.name(), "created user");
            (StatusCode::CREATED, Json(u)).into_response()
        }
        Err(e) => user_error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserChangeRequest {
    #[serde(default)]
    password: Option<String>,
    /// Replaces all access levels.
    #[serde(default)]
    access: Option<BTreeMap<String, Access>>,
}

async fn update_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(change): Json<UserChangeRequest>,
) -> Response {
    let who = match manage_users(&s, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    if let Some(a) = &change.access
        && let Err(e) = identity::validate_access(a)
    {
        return user_error(e);
    }
    let password_hash = match change.password {
        Some(p) => match hash(p).await {
            Ok(h) => Some(h),
            Err(r) => return r,
        },
        None => None,
    };
    let (password, access) = (password_hash.is_some(), change.access.is_some());
    let change = UserChange {
        password_hash,
        access: change.access,
    };
    match s
        .directory
        .identities()
        .update(&name, change, who.name())
        .await
    {
        Ok(u) => {
            tracing::info!(user = %u.name, by = %who.name(), password, access, "changed user");
            Json(u).into_response()
        }
        Err(e) => user_error(e),
    }
}

async fn delete_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let who = match manage_users(&s, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    if who.name() == name {
        return error(StatusCode::CONFLICT, "you cannot delete your own user");
    }
    match s.directory.identities().delete(&name, who.name()).await {
        Ok(()) => {
            tracing::info!(user = %name, by = %who.name(), "deleted user");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => user_error(e),
    }
}
