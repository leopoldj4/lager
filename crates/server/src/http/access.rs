//! Identity checks, repository names, and permissions.
//!
//! Users and their access levels come from the database
//! ([`worktree_core::identity`]); repository names and settings come from
//! the repository catalog in storage. Both apply to the next request
//! without a restart.

use base64::Engine;

use worktree_core::catalog::Catalog;
pub use worktree_core::identity::{AuthError, Identity};
use worktree_core::identity::{NamedUser, PgUsers};
use worktree_core::repo::{Access, RepoSettings};
use worktree_core::storage::StorageError;

/// A push to a missing repository creates it when the user has at least
/// this access to the name (docs/decisions/0005-create-on-push.md).
const CREATE_ON_PUSH: Access = Access::Write;

pub struct Directory {
    users: PgUsers,
    catalog: Catalog,
}

impl Directory {
    pub fn new(users: PgUsers, catalog: Catalog) -> Self {
        Self { users, catalog }
    }

    pub fn users(&self) -> &PgUsers {
        &self.users
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The repository for a client-supplied name.
    pub async fn repo(&self, name: &str) -> Result<Option<RepoSettings>, StorageError> {
        self.catalog.lookup(name).await
    }

    /// Checks HTTP Basic credentials.
    pub async fn authenticate(&self, authorization: Option<&str>) -> Result<Identity, AuthError> {
        let Some(h) = authorization else {
            return Ok(Identity::Anonymous);
        };
        let (user, password) = parse_basic(h).ok_or(AuthError::BadCredentials)?;
        self.sign_in(&user, &password).await
    }

    /// Checks a user name and password from the sign-in form.
    pub async fn sign_in(&self, user: &str, password: &str) -> Result<Identity, AuthError> {
        self.users
            .authenticate(user, password)
            .await?
            .ok_or(AuthError::BadCredentials)
    }

    /// The user with this name and its current access, for web sessions.
    pub async fn lookup(&self, name: &str) -> Result<Option<NamedUser>, AuthError> {
        self.users.lookup(name).await
    }

    /// Whether a push by `who` may create the missing repository `name`.
    /// Anonymous users never create.
    pub fn may_create_on_push(&self, who: &Identity, name: &str) -> bool {
        if *who == Identity::Anonymous {
            return false;
        }
        let probe = RepoSettings::new(name, worktree_core::model::RepoId(uuid::Uuid::nil()));
        self.access(who, &probe) >= CREATE_ON_PUSH
    }

    /// The access level of a user for a repository.
    pub fn access(&self, who: &Identity, repo: &RepoSettings) -> Access {
        let base = who.access(&repo.name);
        if repo.anonymous_read {
            base.max(Access::Read)
        } else {
            base
        }
    }
}

fn parse_basic(h: &str) -> Option<(String, String)> {
    let encoded = h.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, password) = decoded.split_once(':')?;
    Some((user.to_string(), password.to_string()))
}
