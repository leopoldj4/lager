//! Repository lifetime operations that need no cache: creation, deletion,
//! and default-branch changes. Each is one log entry attached with CAS.

use std::time::{Duration, Instant};

use super::push::{Resolution, resolve_unknown_cas};
use crate::model::{
    EntryId, FORMAT_VERSION, Head, IncarnationId, LogEntry, ObjectFormat, Operation, RefName,
    RepoId, RepoState, RequestId, RevisionId, now_ms, request_checksum,
};
use crate::storage::{CasOutcome, Storage, StorageError};

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("repository already exists")]
    Exists,
    #[error("repository not found")]
    NotFound,
    #[error("repository deleted")]
    Deleted,
    #[error("result unknown: {0}")]
    Unknown(String),
    #[error("too many concurrent changes")]
    Conflict,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Creates the head with a conditional request. A head that exists (active
/// or deleted) is never replaced, so delayed requests cannot restore a
/// deleted repository.
pub async fn create_repository(
    storage: &Storage,
    repo: RepoId,
    default_branch: &RefName,
    user: &str,
) -> Result<Head, AdminError> {
    let incarnation = IncarnationId::new();
    let entry = LogEntry {
        format: FORMAT_VERSION,
        repository: repo,
        incarnation,
        id: EntryId::new(),
        previous: None,
        generation: 0,
        request: RequestId::new(),
        request_checksum: request_checksum(user, &[]),
        operation: Operation::Create {
            default_branch: default_branch.clone(),
        },
        refs: vec![],
        packs: vec![],
        user: user.into(),
        rules_version: 0,
        received_at_ms: now_ms(),
    };
    storage.put_entry(&entry).await?;
    let head = Head {
        format: FORMAT_VERSION,
        repository: repo,
        incarnation,
        revision: RevisionId::new(),
        generation: 0,
        latest_entry: entry.id,
        snapshot: None,
        object_format: ObjectFormat::Sha1,
        state: RepoState::Active,
        removal: None,
    };
    for _ in 0..4 {
        match storage.create_head(&head).await {
            CasOutcome::Accepted(_) => return Ok(head),
            CasOutcome::Conflict => {
                // Our earlier attempt may have been applied.
                return match storage.load_head(repo).await? {
                    Some((h, _)) if h.incarnation == incarnation => Ok(head),
                    _ => Err(AdminError::Exists),
                };
            }
            CasOutcome::Unknown(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    match storage.load_head(repo).await? {
        Some((h, _)) if h.incarnation == incarnation => Ok(head),
        Some(_) => Err(AdminError::Exists),
        None => Err(AdminError::Unknown("head creation not confirmed".into())),
    }
}

/// Ensures the head exists; returns true if this call created it.
pub async fn ensure_repository(
    storage: &Storage,
    repo: RepoId,
    default_branch: &RefName,
) -> Result<bool, AdminError> {
    if storage.load_head(repo).await?.is_some() {
        return Ok(false);
    }
    match create_repository(storage, repo, default_branch, "rustgit").await {
        Ok(_) => Ok(true),
        Err(AdminError::Exists) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Appends one entry without ref changes, with retries on conflicts.
pub async fn append_entry(
    storage: &Storage,
    repo: RepoId,
    operation: Operation,
    user: &str,
) -> Result<Head, AdminError> {
    let request = RequestId::new();
    for attempt in 0..16u32 {
        let Some((head, etag)) = storage.load_head(repo).await? else {
            return Err(AdminError::NotFound);
        };
        if head.state == RepoState::Deleted {
            return Err(AdminError::Deleted);
        }
        let entry = LogEntry {
            format: FORMAT_VERSION,
            repository: repo,
            incarnation: head.incarnation,
            id: EntryId::new(),
            previous: Some(head.latest_entry),
            generation: head.generation + 1,
            request,
            request_checksum: request_checksum(user, &[]),
            operation: operation.clone(),
            refs: vec![],
            packs: vec![],
            user: user.into(),
            rules_version: 0,
            received_at_ms: now_ms(),
        };
        storage.put_entry(&entry).await?;
        let next = head.advance(&entry);
        match storage.replace_head(&next, &etag).await {
            CasOutcome::Accepted(_) => return Ok(next),
            CasOutcome::Conflict => {}
            CasOutcome::Unknown(_) => {
                let until = Instant::now() + Duration::from_secs(30);
                match resolve_unknown_cas(storage, &next, &etag, entry.id, until).await {
                    Resolution::Accepted(_) => return Ok(next),
                    Resolution::Rejected => {}
                    Resolution::Unknown(m) => return Err(AdminError::Unknown(m)),
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20 << attempt.min(6))).await;
    }
    Err(AdminError::Conflict)
}

/// Marks the repository deleted with a tombstone head.
pub async fn delete_repository(
    storage: &Storage,
    repo: RepoId,
    user: &str,
) -> Result<Head, AdminError> {
    append_entry(storage, repo, Operation::Delete, user).await
}

pub async fn set_default_branch(
    storage: &Storage,
    repo: RepoId,
    branch: RefName,
    user: &str,
) -> Result<Head, AdminError> {
    append_entry(storage, repo, Operation::DefaultBranch { branch }, user).await
}
