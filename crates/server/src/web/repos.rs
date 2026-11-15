//! Repositories of the web interface: the list, creation, and the
//! settings: rename, visibility, and deletion. Only the owner, the user
//! whose name the repository starts with
//! (docs/decisions/0018-repositories-under-users.md), changes the settings;
//! grants of other users do not reach them.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ops::{Empty, Op, OpError, readable};
use crate::http::AppInner;
use crate::repository::RepoError;
use lager_core::catalog::CatalogError;
use lager_core::identity::Identity;
use lager_core::repo::{Access, RepoSettings, validate_repo_name};

#[derive(Serialize, TS)]
pub struct RepoSummary {
    name: String,
    default_branch: String,
    /// Anyone may read it, also without signing in.
    anonymous_read: bool,
    /// The GitHub repository of a mirror.
    mirror: Option<String>,
}

impl From<RepoSettings> for RepoSummary {
    fn from(r: RepoSettings) -> Self {
        Self {
            name: r.name,
            default_branch: r.default_branch,
            anonymous_read: r.anonymous_read,
            mirror: r.mirror,
        }
    }
}

#[derive(Serialize, TS)]
pub struct RepoList {
    repos: Vec<RepoSummary>,
}

/// A repository after its creation or a change to its settings.
#[derive(Serialize, TS)]
pub struct RepoChanged {
    repo: RepoSummary,
}

/// The repositories the user may read.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReposOp {}

impl Op for ReposOp {
    type Answer = RepoList;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<RepoList, OpError> {
        list(s, who).await
    }
}

/// Creates an empty repository; needs a session.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct RepoCreateOp {
    name: String,
    /// Anyone may read it, also without signing in.
    public: Option<bool>,
}

impl Op for RepoCreateOp {
    type Answer = RepoChanged;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<RepoChanged, OpError> {
        create(s, who, &self.name, self.public.unwrap_or(false)).await
    }
}

// The settings need a session and the repository's owner.

/// Gives the repository a new full name under the same owner.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RepoRenameOp {
    repo: String,
    name: String,
}

impl Op for RepoRenameOp {
    type Answer = RepoChanged;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<RepoChanged, OpError> {
        rename(s, who, &self.repo, self.name.trim()).await
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RepoVisibilityOp {
    repo: String,
    /// Anyone may read it, also without signing in.
    public: bool,
}

impl Op for RepoVisibilityOp {
    type Answer = RepoChanged;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<RepoChanged, OpError> {
        set_visibility(s, who, &self.repo, self.public).await
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RepoDeleteOp {
    repo: String,
}

impl Op for RepoDeleteOp {
    type Answer = Empty;

    async fn run(self, s: &AppInner, who: &Identity) -> Result<Empty, OpError> {
        delete(s, who, &self.repo).await
    }
}

async fn list(s: &AppInner, who: &Identity) -> Result<RepoList, OpError> {
    let repos = s
        .directory
        .catalog()
        .list()
        .await
        .map_err(|e| OpError::from(RepoError::from(e)))?
        .into_iter()
        .filter(|r| r.state == lager_core::model::RepoState::Active)
        .map(|r| r.settings())
        .filter(|r| s.directory.access(who, r) >= Access::Read)
        .map(RepoSummary::from)
        .collect();
    Ok(RepoList { repos })
}

/// Creates a repository as a push to a missing name would
/// (docs/decisions/0005-create-on-push.md), with the chosen visibility.
async fn create(
    s: &AppInner,
    who: &Identity,
    name: &str,
    public: bool,
) -> Result<RepoChanged, OpError> {
    if *who == Identity::Anonymous {
        return Err(OpError::new("unauthorized", "sign in first"));
    }
    validate_repo_name(name).map_err(|e| OpError::bad(e.to_string()))?;
    if !s.directory.may_create_on_push(who, name) {
        return Err(OpError::new(
            "forbidden",
            format!("you cannot create repositories under {name}"),
        ));
    }
    let mut settings = RepoSettings::new(name, lager_core::model::RepoId::new());
    settings.anonymous_read = public;
    let repo = match s.directory.catalog().create(&settings, who.name()).await {
        Ok(r) => r,
        Err(CatalogError::Exists(_)) => {
            return Err(OpError::new("exists", format!("{name} exists already")));
        }
        Err(CatalogError::Invalid(m)) => return Err(OpError::bad(m)),
        Err(CatalogError::Storage(e)) => return Err(RepoError::from(e).into()),
        Err(e) => {
            tracing::warn!(error = %e, "repository catalog unavailable");
            return Err(OpError::new(
                "unavailable",
                "repository catalog unavailable; retry later",
            ));
        }
    };
    s.registry.svc.metrics.inc("repositories_created_on_web");
    tracing::info!(repo = %name, id = %repo.id, user = %who.name(), public, "created repository on the web");
    Ok(RepoChanged {
        repo: RepoSummary {
            name: repo.name,
            default_branch: repo.default_branch,
            anonymous_read: repo.anonymous_read,
            mirror: None,
        },
    })
}

async fn rename(
    s: &AppInner,
    who: &Identity,
    name: &str,
    new_name: &str,
) -> Result<RepoChanged, OpError> {
    let repo = owned(s, who, name).await?;
    validate_repo_name(new_name).map_err(|e| OpError::bad(e.to_string()))?;
    if owner(new_name) != owner(name) {
        return Err(OpError::bad(format!(
            "the new name must start with {}/",
            owner(name)
        )));
    }
    let renamed = s
        .directory
        .catalog()
        .rename(name, new_name)
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
    tracing::info!(repo = %name, new_name, id = %repo.id, user = who.name(), "renamed repository on the web");
    Ok(RepoChanged {
        repo: renamed.into(),
    })
}

async fn set_visibility(
    s: &AppInner,
    who: &Identity,
    name: &str,
    public: bool,
) -> Result<RepoChanged, OpError> {
    let id = owned(s, who, name).await?.id;
    let changed = s
        .directory
        .catalog()
        .update(name, |r| {
            if r.repository == id {
                r.anonymous_read = public;
            }
        })
        .await
        .map_err(catalog_error)?;
    tracing::info!(repo = %name, user = who.name(), public, "changed repository visibility on the web");
    Ok(RepoChanged {
        repo: changed.into(),
    })
}

async fn delete(s: &AppInner, who: &Identity, name: &str) -> Result<Empty, OpError> {
    let repo = owned(s, who, name).await?;
    let user = who.name();
    let id = s
        .directory
        .catalog()
        .delete(name, user)
        .await
        .map_err(catalog_error)?;
    if repo.mirror.is_some()
        && let Some(github) = &s.github
        && let Err(e) = github.forget(id).await
    {
        // The next sync finds the repository deleted and removes it.
        tracing::warn!(repo = %name, error = %e, "mirror not removed");
    }
    s.registry.svc.metrics.inc("repositories_deleted_on_web");
    tracing::info!(repo = %name, %id, user, "deleted repository on the web");
    Ok(Empty {})
}

/// The repository if `who` owns it. Others who may read it learn that they
/// may not change it; others see no repository.
async fn owned(s: &AppInner, who: &Identity, name: &str) -> Result<RepoSettings, OpError> {
    if *who == Identity::Anonymous {
        return Err(OpError::new("unauthorized", "sign in first"));
    }
    let repo = readable(s, who, name).await?;
    if owner(name) != who.name() {
        return Err(OpError::new(
            "forbidden",
            format!("only {} changes the settings of {name}", owner(name)),
        ));
    }
    Ok(repo)
}

fn owner(name: &str) -> &str {
    name.split('/').next().unwrap_or_default()
}

fn catalog_error(e: CatalogError) -> OpError {
    match e {
        CatalogError::Exists(n) => OpError::new("exists", format!("{n} exists already")),
        CatalogError::NotFound(_) => OpError::not_found("repository"),
        CatalogError::Invalid(m) => OpError::bad(m),
        CatalogError::Conflict => {
            OpError::new("conflict", "the repository changed at the same time; retry")
        }
        CatalogError::Storage(e) => RepoError::from(e).into(),
        e => {
            tracing::warn!(error = %e, "repository catalog unavailable");
            OpError::new("unavailable", "repository catalog unavailable; retry later")
        }
    }
}
