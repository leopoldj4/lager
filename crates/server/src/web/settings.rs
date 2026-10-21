//! Repository settings of the web interface: rename, visibility, and
//! deletion. Only the owner, the user whose name the repository starts
//! with (docs/decisions/0018-repositories-under-users.md), changes them;
//! grants of other users do not reach them.

use serde_json::{Value, json};

use super::read::{Query, ReadError, RepoSummary, readable};
use crate::http::AppInner;
use crate::repository::RepoError;
use lager_core::catalog::CatalogError;
use lager_core::identity::Identity;
use lager_core::repo::{RepoSettings, validate_repo_name};

pub async fn run(s: &AppInner, who: &Identity, q: Query) -> Result<Value, ReadError> {
    let name = match &q {
        Query::RepoRename { repo, .. }
        | Query::RepoVisibility { repo, .. }
        | Query::RepoDelete { repo } => repo.clone(),
        _ => return Err(ReadError::bad("not a repository settings query")),
    };
    let repo = owned(s, who, &name).await?;
    let catalog = s.directory.catalog();
    let user = who.name();
    match q {
        Query::RepoRename { name: new_name, .. } => {
            let new_name = new_name.trim();
            validate_repo_name(new_name).map_err(|e| ReadError::bad(e.to_string()))?;
            if owner(new_name) != owner(&name) {
                return Err(ReadError::bad(format!(
                    "the new name must start with {}/",
                    owner(&name)
                )));
            }
            let renamed = catalog
                .rename(&name, new_name)
                .await
                .map_err(catalog_error)?;
            if repo.mirror.is_some()
                && let Some(github) = &s.github
                && let Err(e) = github.db.rename(repo.id, new_name).await
            {
                // The next sync finds no mirror under the old name and
                // stops; the user mirrors it again.
                tracing::warn!(repo = %new_name, error = %e, "mirror not renamed");
            }
            s.registry.svc.metrics.inc("repositories_renamed_on_web");
            tracing::info!(repo = %name, new_name, id = %repo.id, user, "renamed repository on the web");
            Ok(json!({ "repo": RepoSummary::from(renamed) }))
        }
        Query::RepoVisibility { public, .. } => {
            let id = repo.id;
            let changed = catalog
                .update(&name, |r| {
                    if r.repository == id {
                        r.anonymous_read = public;
                    }
                })
                .await
                .map_err(catalog_error)?;
            tracing::info!(repo = %name, user, public, "changed repository visibility on the web");
            Ok(json!({ "repo": RepoSummary::from(changed) }))
        }
        Query::RepoDelete { .. } => {
            let id = catalog.delete(&name, user).await.map_err(catalog_error)?;
            if repo.mirror.is_some()
                && let Some(github) = &s.github
                && let Err(e) = github.db.remove(id).await
            {
                // The next sync finds the repository deleted and removes it.
                tracing::warn!(repo = %name, error = %e, "mirror not removed");
            }
            s.registry.svc.metrics.inc("repositories_deleted_on_web");
            tracing::info!(repo = %name, %id, user, "deleted repository on the web");
            Ok(json!({}))
        }
        _ => unreachable!(),
    }
}

/// The repository if `who` owns it. Others who may read it learn that they
/// may not change it; others see no repository.
async fn owned(s: &AppInner, who: &Identity, name: &str) -> Result<RepoSettings, ReadError> {
    if *who == Identity::Anonymous {
        return Err(ReadError::new("unauthorized", "sign in first"));
    }
    let repo = readable(s, who, name).await?;
    if owner(name) != who.name() {
        return Err(ReadError::new(
            "forbidden",
            format!("only {} changes the settings of {name}", owner(name)),
        ));
    }
    Ok(repo)
}

fn owner(name: &str) -> &str {
    name.split('/').next().unwrap_or_default()
}

fn catalog_error(e: CatalogError) -> ReadError {
    match e {
        CatalogError::Exists(n) => ReadError::new("exists", format!("{n} exists already")),
        CatalogError::NotFound(_) => ReadError::not_found("repository"),
        CatalogError::Invalid(m) => ReadError::bad(m),
        CatalogError::Conflict => {
            ReadError::new("conflict", "the repository changed at the same time; retry")
        }
        CatalogError::Storage(e) => RepoError::from(e).into(),
        e => {
            tracing::warn!(error = %e, "repository catalog unavailable");
            ReadError::new("unavailable", "repository catalog unavailable; retry later")
        }
    }
}
