//! The repository catalog in the storage service.
//!
//! `<prefix>catalog/<sha256 of the name>.json` maps one repository name to the
//! UUID of its current repository. The name is hashed: a client path never
//! becomes a storage key. A record is created with a conditional request and
//! changed only with CAS. A reused name gets a new repository UUID; the old
//! repository keeps its tombstone head, so delayed requests cannot restore it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::admin::{self, AdminError};
use crate::model::{FORMAT_VERSION, RepoId, RepoState, RevisionId, branch_ref};
use crate::repo::{RefRule, RepoSettings, validate_repo_name};
use crate::storage::{CasOutcome, ETag, Storage, StorageError, Versioned, WriteMode};

/// Unknown fields are ignored: older records also have a `customer`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRecord {
    pub format: u32,
    pub name: String,
    pub repository: RepoId,
    pub state: RepoState,
    /// Changes on every record update.
    pub revision: RevisionId,
    pub default_branch: String,
    pub anonymous_read: bool,
    pub ref_rules: Vec<RefRule>,
}

impl CatalogRecord {
    pub fn settings(&self) -> RepoSettings {
        RepoSettings {
            name: self.name.clone(),
            id: self.repository,
            default_branch: self.default_branch.clone(),
            anonymous_read: self.anonymous_read,
            ref_rules: self.ref_rules.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("repository {0} exists")]
    Exists(String),
    #[error("repository {0} not found")]
    NotFound(String),
    #[error("invalid repository settings: {0}")]
    Invalid(String),
    #[error("concurrent catalog change; retry")]
    Conflict,
    #[error("result unknown: {0}")]
    Unknown(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Admin(#[from] AdminError),
}

type CacheEntry = (Instant, Option<(CatalogRecord, ETag)>);

pub struct Catalog {
    storage: Storage,
    ttl: Duration,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl Catalog {
    pub fn new(storage: Storage, ttl: Duration) -> Self {
        Self {
            storage,
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn prefix(&self) -> String {
        format!("{}catalog/", self.storage.prefix())
    }

    pub fn key(&self, name: &str) -> String {
        format!(
            "{}{}.json",
            self.prefix(),
            hex::encode(Sha256::digest(name.as_bytes()))
        )
    }

    /// Reads the current record from storage.
    pub async fn record(&self, name: &str) -> Result<Option<(CatalogRecord, ETag)>, StorageError> {
        let key = self.key(name);
        match self
            .storage
            .read_versioned::<CatalogRecord>(&key, None)
            .await?
        {
            Versioned::Found(r, etag) => {
                check(&key, name, &r)?;
                Ok(Some((r, etag)))
            }
            Versioned::Missing | Versioned::Unchanged => Ok(None),
        }
    }

    /// The active repository for a name. Lookups are reused for the cache
    /// time; after that a conditional read checks for changes.
    pub async fn lookup(&self, name: &str) -> Result<Option<RepoSettings>, StorageError> {
        let cached = self.cache.lock().unwrap().get(name).cloned();
        if let Some((at, value)) = &cached
            && at.elapsed() < self.ttl
        {
            return Ok(active(value));
        }
        let key = self.key(name);
        let known = cached
            .as_ref()
            .and_then(|(_, v)| v.as_ref().map(|(_, e)| e));
        let value = match self
            .storage
            .read_versioned::<CatalogRecord>(&key, known)
            .await?
        {
            Versioned::Unchanged => cached.and_then(|(_, v)| v),
            Versioned::Missing => None,
            Versioned::Found(r, etag) => {
                check(&key, name, &r)?;
                Some((r, etag))
            }
        };
        let result = active(&value);
        self.cache
            .lock()
            .unwrap()
            .insert(name.to_string(), (Instant::now(), value));
        Ok(result)
    }

    /// Creates a repository and its catalog record. `settings.id` is ignored:
    /// every creation gets a new repository UUID.
    pub async fn create(
        &self,
        settings: &RepoSettings,
        user: &str,
    ) -> Result<RepoSettings, CatalogError> {
        let branch = branch_ref(&settings.default_branch)
            .map_err(|e| CatalogError::Invalid(e.to_string()))?;
        let mode = self.free_name(&settings.name).await?;
        let id = RepoId::new();
        admin::create_repository(&self.storage, id, &branch, user).await?;
        // A lost race leaves an unreferenced repository head; nothing serves it.
        self.insert(settings, id, mode).await
    }

    /// Gives an existing repository, such as one restored from a backup, a
    /// name. `settings.id` is the repository; its head must be active.
    pub async fn attach(&self, settings: &RepoSettings) -> Result<RepoSettings, CatalogError> {
        branch_ref(&settings.default_branch).map_err(|e| CatalogError::Invalid(e.to_string()))?;
        match self.storage.load_head(settings.id).await? {
            Some((h, _)) if h.state == RepoState::Active => {}
            Some(_) => return Err(AdminError::Deleted.into()),
            None => return Err(AdminError::NotFound.into()),
        }
        let mode = self.free_name(&settings.name).await?;
        self.insert(settings, settings.id, mode).await
    }

    /// How to write the record of a new repository with this name: a name
    /// without an active repository can be taken.
    async fn free_name(&self, name: &str) -> Result<WriteMode, CatalogError> {
        validate_repo_name(name).map_err(|e| CatalogError::Invalid(e.to_string()))?;
        Ok(match self.record(name).await? {
            Some((r, _)) if r.state == RepoState::Active => {
                return Err(CatalogError::Exists(name.into()));
            }
            Some((_, etag)) => WriteMode::Update(etag.0),
            None => WriteMode::Create,
        })
    }

    async fn insert(
        &self,
        settings: &RepoSettings,
        id: RepoId,
        mode: WriteMode,
    ) -> Result<RepoSettings, CatalogError> {
        for rule in &settings.ref_rules {
            crate::repo::validate_ref_rule(rule)
                .map_err(|e| CatalogError::Invalid(e.to_string()))?;
        }
        let record = CatalogRecord {
            format: FORMAT_VERSION,
            name: settings.name.clone(),
            repository: id,
            state: RepoState::Active,
            revision: RevisionId::new(),
            default_branch: settings.default_branch.clone(),
            anonymous_read: settings.anonymous_read,
            ref_rules: settings.ref_rules.clone(),
        };
        self.write(&record, mode, || {
            CatalogError::Exists(settings.name.clone())
        })
        .await?;
        Ok(record.settings())
    }

    /// The active repository for a name, created with default settings if
    /// there is none; true if this call created it. Reads the record, not the
    /// lookup cache: another server may have created it within the cache time.
    pub async fn ensure(
        &self,
        name: &str,
        user: &str,
    ) -> Result<(RepoSettings, bool), CatalogError> {
        for _ in 0..4 {
            if let Some((r, _)) = self.record(name).await?
                && r.state == RepoState::Active
            {
                self.cache.lock().unwrap().remove(name);
                return Ok((r.settings(), false));
            }
            match self
                .create(&RepoSettings::new(name, RepoId::new()), user)
                .await
            {
                Ok(r) => return Ok((r, true)),
                // Created concurrently: read the winner.
                Err(CatalogError::Exists(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(CatalogError::Conflict)
    }

    /// Marks the repository deleted, then the record. The tombstone head
    /// comes first so that the repository stops being served even if the
    /// record change fails.
    pub async fn delete(&self, name: &str, user: &str) -> Result<RepoId, CatalogError> {
        let Some((mut record, etag)) = self.record(name).await? else {
            return Err(CatalogError::NotFound(name.into()));
        };
        if record.state == RepoState::Deleted {
            return Err(CatalogError::NotFound(name.into()));
        }
        match admin::delete_repository(&self.storage, record.repository, user).await {
            Ok(_) | Err(AdminError::Deleted) => {}
            Err(e) => return Err(e.into()),
        }
        record.state = RepoState::Deleted;
        record.revision = RevisionId::new();
        self.write(&record, WriteMode::Update(etag.0), || {
            CatalogError::Conflict
        })
        .await?;
        Ok(record.repository)
    }

    /// Changes the default branch in the history (Git `HEAD`), then in the
    /// record.
    pub async fn set_default_branch(
        &self,
        name: &str,
        branch: &str,
        user: &str,
    ) -> Result<(RepoSettings, u64), CatalogError> {
        let branch_name = branch_ref(branch).map_err(|e| CatalogError::Invalid(e.to_string()))?;
        let Some((record, _)) = self.record(name).await? else {
            return Err(CatalogError::NotFound(name.into()));
        };
        if record.state != RepoState::Active {
            return Err(CatalogError::NotFound(name.into()));
        }
        let head =
            admin::set_default_branch(&self.storage, record.repository, branch_name, user).await?;
        let id = record.repository;
        let repo = self
            .update(name, |r| {
                if r.repository == id {
                    r.default_branch = branch.to_string();
                }
            })
            .await?;
        Ok((repo, head.generation))
    }

    /// Changes the settings of an active repository with CAS.
    pub async fn update(
        &self,
        name: &str,
        change: impl Fn(&mut CatalogRecord),
    ) -> Result<RepoSettings, CatalogError> {
        for _ in 0..8 {
            let Some((mut record, etag)) = self.record(name).await? else {
                return Err(CatalogError::NotFound(name.into()));
            };
            if record.state != RepoState::Active {
                return Err(CatalogError::NotFound(name.into()));
            }
            let id = record.repository;
            change(&mut record);
            record.repository = id;
            record.name = name.into();
            record.revision = RevisionId::new();
            for rule in &record.ref_rules {
                crate::repo::validate_ref_rule(rule)
                    .map_err(|e| CatalogError::Invalid(e.to_string()))?;
            }
            match self
                .write(&record, WriteMode::Update(etag.0), || {
                    CatalogError::Conflict
                })
                .await
            {
                Ok(()) => return Ok(record.settings()),
                Err(CatalogError::Conflict) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(CatalogError::Conflict)
    }

    pub async fn list(&self) -> Result<Vec<CatalogRecord>, StorageError> {
        let prefix = self.prefix();
        let objects = self
            .storage
            .backend()
            .list(prefix.trim_end_matches('/'))
            .await
            .map_err(|e| StorageError::Other(e.to_string()))?;
        let mut out = Vec::new();
        for o in objects {
            let r: CatalogRecord = self.storage.get_record(&o.key).await?;
            if o.key == self.key(&r.name) {
                out.push(r);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn write(
        &self,
        record: &CatalogRecord,
        mode: WriteMode,
        conflict: impl Fn() -> CatalogError,
    ) -> Result<(), CatalogError> {
        let key = self.key(&record.name);
        let revision = record.revision;
        let outcome = self
            .storage
            .write_versioned(&key, record, mode, |r: &CatalogRecord| {
                r.revision == revision
            })
            .await;
        self.cache.lock().unwrap().remove(&record.name);
        match outcome {
            CasOutcome::Accepted(_) => Ok(()),
            CasOutcome::Conflict => Err(conflict()),
            // Not repeated: a lost answer is resolved by reading the record.
            CasOutcome::Unknown(m) => match self.record(&record.name).await {
                Ok(Some((r, _))) if r.revision == revision => Ok(()),
                Ok(_) => Err(conflict()),
                Err(_) => Err(CatalogError::Unknown(m)),
            },
        }
    }
}

fn active(value: &Option<(CatalogRecord, ETag)>) -> Option<RepoSettings> {
    value
        .as_ref()
        .filter(|(r, _)| r.state == RepoState::Active)
        .map(|(r, _)| r.settings())
}

fn check(key: &str, name: &str, r: &CatalogRecord) -> Result<(), StorageError> {
    if r.format != FORMAT_VERSION {
        return Err(StorageError::Corrupt(format!(
            "{key}: unsupported format {}",
            r.format
        )));
    }
    if r.name != name {
        return Err(StorageError::Corrupt(format!(
            "{key}: record for another name"
        )));
    }
    Ok(())
}
