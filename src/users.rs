//! Users in the storage service, managed through the admin API
//! (docs/decisions/0004-users-in-storage.md).
//!
//! `<prefix>users/<sha256 of the name>.json` holds one user's salted password
//! hash and access levels. Records follow the catalog rules: conditional
//! creation, then CAS only. Deletion marks the record deleted; a reused name
//! replaces it. Users in the configuration file take precedence and are not
//! stored here.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Access, validate_repo_name};
use crate::model::{FORMAT_VERSION, RevisionId, now_ms};
use crate::storage::{CasOutcome, ETag, Storage, StorageError, Versioned, WriteMode};

pub const MIN_PASSWORD_CHARS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserState {
    Active,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserRecord {
    pub format: u32,
    pub name: String,
    pub state: UserState,
    /// Changes on every record update.
    pub revision: RevisionId,
    /// From [`crate::http::password::PasswordHash`].
    pub password_hash: String,
    /// Repository name (or "*") to access level.
    pub access: BTreeMap<String, Access>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub updated_by: String,
}

#[derive(Debug, thiserror::Error)]
pub enum UserError {
    #[error("user {0} exists")]
    Exists(String),
    #[error("user {0} not found")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("concurrent user change; retry")]
    Conflict,
    #[error("result unknown: {0}")]
    Unknown(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// User names appear in HTTP Basic credentials and in log entries.
pub fn validate_user_name(name: &str) -> Result<(), UserError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['.', '-'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(UserError::Invalid(format!(
            "invalid user name {name:?}: use 1 to 64 letters, digits, '.', '_', or '-'"
        )))
    }
}

pub fn validate_password(password: &str) -> Result<(), UserError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(UserError::Invalid(format!(
            "passwords need at least {MIN_PASSWORD_CHARS} characters"
        )));
    }
    Ok(())
}

pub fn validate_access(access: &BTreeMap<String, Access>) -> Result<(), UserError> {
    for name in access.keys() {
        if name != "*" {
            validate_repo_name(name).map_err(|e| UserError::Invalid(e.to_string()))?;
        }
    }
    Ok(())
}

type CacheEntry = (Instant, Option<(UserRecord, ETag)>);

pub struct UserStore {
    storage: Storage,
    ttl: Duration,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl UserStore {
    pub fn new(storage: Storage, ttl: Duration) -> Self {
        Self {
            storage,
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn prefix(&self) -> String {
        format!("{}users/", self.storage.prefix())
    }

    pub fn key(&self, name: &str) -> String {
        format!(
            "{}{}.json",
            self.prefix(),
            hex::encode(Sha256::digest(name.as_bytes()))
        )
    }

    /// Reads the current record from storage.
    pub async fn record(&self, name: &str) -> Result<Option<(UserRecord, ETag)>, StorageError> {
        let key = self.key(name);
        match self
            .storage
            .read_versioned::<UserRecord>(&key, None)
            .await?
        {
            Versioned::Found(r, etag) => {
                check(&key, name, &r)?;
                Ok(Some((r, etag)))
            }
            Versioned::Missing | Versioned::Unchanged => Ok(None),
        }
    }

    /// The active user for a name. Lookups are reused for the cache time;
    /// after that a conditional read checks for changes. Another server can
    /// therefore accept a changed password or access for up to that time.
    pub async fn lookup(&self, name: &str) -> Result<Option<UserRecord>, StorageError> {
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
            .read_versioned::<UserRecord>(&key, known)
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

    /// Creates a user, or replaces a deleted one with the same name.
    pub async fn create(
        &self,
        name: &str,
        password_hash: String,
        access: BTreeMap<String, Access>,
        by: &str,
    ) -> Result<UserRecord, UserError> {
        validate_user_name(name)?;
        validate_access(&access)?;
        let mode = match self.record(name).await? {
            Some((r, _)) if r.state == UserState::Active => {
                return Err(UserError::Exists(name.into()));
            }
            Some((_, etag)) => WriteMode::Update(etag.0),
            None => WriteMode::Create,
        };
        let now = now_ms();
        let record = UserRecord {
            format: FORMAT_VERSION,
            name: name.into(),
            state: UserState::Active,
            revision: RevisionId::new(),
            password_hash,
            access,
            created_ms: now,
            updated_ms: now,
            updated_by: by.into(),
        };
        self.write(&record, mode, || UserError::Exists(name.into()))
            .await?;
        Ok(record)
    }

    /// Changes an active user with CAS.
    pub async fn update(
        &self,
        name: &str,
        by: &str,
        change: impl Fn(&mut UserRecord),
    ) -> Result<UserRecord, UserError> {
        for _ in 0..8 {
            let Some((mut record, etag)) = self.record(name).await? else {
                return Err(UserError::NotFound(name.into()));
            };
            if record.state != UserState::Active {
                return Err(UserError::NotFound(name.into()));
            }
            change(&mut record);
            record.name = name.into();
            record.revision = RevisionId::new();
            record.updated_ms = now_ms();
            record.updated_by = by.into();
            validate_access(&record.access)?;
            match self
                .write(&record, WriteMode::Update(etag.0), || UserError::Conflict)
                .await
            {
                Ok(()) => return Ok(record),
                Err(UserError::Conflict) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(UserError::Conflict)
    }

    /// Marks the user deleted. Its credentials stop working at once on this
    /// server and within the cache time on others.
    pub async fn delete(&self, name: &str, by: &str) -> Result<(), UserError> {
        self.update(name, by, |r| {
            r.state = UserState::Deleted;
            r.access.clear();
        })
        .await
        .map(|_| ())
    }

    /// Active users, sorted by name.
    pub async fn list(&self) -> Result<Vec<UserRecord>, StorageError> {
        let prefix = self.prefix();
        let objects = self
            .storage
            .backend()
            .list(prefix.trim_end_matches('/'))
            .await
            .map_err(|e| StorageError::Other(e.to_string()))?;
        let mut out = Vec::new();
        for o in objects {
            let r: UserRecord = self.storage.get_record(&o.key).await?;
            if o.key == self.key(&r.name) && r.state == UserState::Active {
                out.push(r);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn write(
        &self,
        record: &UserRecord,
        mode: WriteMode,
        conflict: impl Fn() -> UserError,
    ) -> Result<(), UserError> {
        let key = self.key(&record.name);
        let revision = record.revision;
        let outcome = self
            .storage
            .write_versioned(&key, record, mode, |r: &UserRecord| r.revision == revision)
            .await;
        self.cache.lock().unwrap().remove(&record.name);
        match outcome {
            CasOutcome::Accepted(_) => Ok(()),
            CasOutcome::Conflict => Err(conflict()),
            // Not repeated: a lost answer is resolved by reading the record.
            CasOutcome::Unknown(m) => match self.record(&record.name).await {
                Ok(Some((r, _))) if r.revision == revision => Ok(()),
                Ok(_) => Err(conflict()),
                Err(_) => Err(UserError::Unknown(m)),
            },
        }
    }
}

fn active(value: &Option<(UserRecord, ETag)>) -> Option<UserRecord> {
    value
        .as_ref()
        .filter(|(r, _)| r.state == UserState::Active)
        .map(|(r, _)| r.clone())
}

fn check(key: &str, name: &str, r: &UserRecord) -> Result<(), StorageError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::ObjectStoreBackend;
    use std::sync::Arc;

    fn store() -> UserStore {
        let storage = Storage::new(Arc::new(ObjectStoreBackend::in_memory()), "t/");
        UserStore::new(storage, Duration::ZERO)
    }

    #[test]
    fn user_names_reject_separators_and_leading_punctuation() {
        for ok in ["leo", "ci-bot", "a.b_c", "X1"] {
            validate_user_name(ok).unwrap();
        }
        for bad in ["", "a:b", "a b", "a/b", ".x", "-x", "ü", &"x".repeat(65)] {
            assert!(validate_user_name(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_user_is_created_changed_deleted_and_the_name_reused() {
        let s = store();
        let access = BTreeMap::from([("demo".to_string(), Access::Write)]);
        let first = s
            .create("erin", "h1".into(), access.clone(), "admin")
            .await
            .unwrap();
        assert!(matches!(
            s.create("erin", "h2".into(), access.clone(), "admin").await,
            Err(UserError::Exists(_))
        ));
        let changed = s
            .update("erin", "admin", |r| r.password_hash = "h3".into())
            .await
            .unwrap();
        assert_eq!(changed.password_hash, "h3");
        assert_ne!(changed.revision, first.revision);
        assert_eq!(changed.created_ms, first.created_ms);
        assert_eq!(s.list().await.unwrap(), vec![changed]);

        s.delete("erin", "admin").await.unwrap();
        assert_eq!(s.lookup("erin").await.unwrap(), None);
        assert!(s.list().await.unwrap().is_empty());
        assert!(matches!(
            s.delete("erin", "admin").await,
            Err(UserError::NotFound(_))
        ));
        let again = s
            .create("erin", "h4".into(), BTreeMap::new(), "admin")
            .await
            .unwrap();
        assert_eq!(s.lookup("erin").await.unwrap(), Some(again));
        // The key is a digest, never the client-supplied name.
        assert!(!s.key("erin").contains("erin"));
    }

    #[tokio::test]
    async fn access_to_invalid_repository_names_is_rejected() {
        let s = store();
        let access = BTreeMap::from([("../x".to_string(), Access::Read)]);
        assert!(matches!(
            s.create("erin", "h".into(), access, "admin").await,
            Err(UserError::Invalid(_))
        ));
    }
}
