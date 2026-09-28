//! Typed access to repository records in an S3-compatible storage service.
//!
//! Layout (ARCHITECTURE.md §5):
//! ```text
//! <prefix>repos/<repository-id>/head.json
//! <prefix>repos/<repository-id>/entries/<entry-id>.json
//! <prefix>repos/<repository-id>/packs/<sha256>.pack
//! <prefix>repos/<repository-id>/snapshots/<snapshot-id>.json
//! <prefix>repos/<repository-id>/receipts/<receipt-id>.json
//! ```
//! Only `head.json` changes in place, and only with CAS.

pub mod backend;
pub mod fault;
pub mod provider_check;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::model::{
    EntryId, Head, LogEntry, PackRef, Receipt, RepoId, RequestId, Snapshot, SnapshotId,
};
pub use backend::{Backend, BackendError, ObjectInfo, ObjectStoreBackend, WriteMode};

/// Identifies one version of the head object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ETag(pub String);

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// No answer from the service. Fail new reads and writes.
    #[error("storage unavailable: {0}")]
    Unavailable(String),
    #[error("missing stored object {0}")]
    Missing(String),
    /// Damaged data, a checksum mismatch, or an unsupported record format.
    #[error("damaged stored data: {0}")]
    Corrupt(String),
    #[error("storage error: {0}")]
    Other(String),
}

impl StorageError {
    fn from_backend(key: &str, e: BackendError) -> Self {
        match e {
            BackendError::NotFound => StorageError::Missing(key.to_string()),
            BackendError::Unavailable(m) => StorageError::Unavailable(m),
            other => StorageError::Other(format!("{key}: {other}")),
        }
    }
}

pub enum Versioned<T> {
    Missing,
    /// The object still has the ETag the caller supplied.
    Unchanged,
    Found(T, ETag),
}

pub enum HeadRead {
    Missing,
    /// The head still has the ETag the caller supplied.
    Unchanged,
    Found {
        head: Head,
        etag: ETag,
    },
}

/// The result of a conditional head write.
#[derive(Debug)]
pub enum CasOutcome {
    Accepted(ETag),
    Conflict,
    /// No answer. The write may or may not have been applied.
    Unknown(String),
}

#[derive(Clone, Debug)]
pub struct Storage {
    backend: Arc<dyn Backend>,
    prefix: String,
    verify_uploads: bool,
    /// Limits simultaneous pack uploads.
    uploads: Option<Arc<tokio::sync::Semaphore>>,
    stats: Arc<StorageStats>,
}

/// Pack transfers, for measurements of storage use and cost.
#[derive(Debug, Default)]
pub struct StorageStats {
    pub pack_uploads: std::sync::atomic::AtomicU64,
    pub uploaded_bytes: std::sync::atomic::AtomicU64,
    pub pack_downloads: std::sync::atomic::AtomicU64,
    pub downloaded_bytes: std::sync::atomic::AtomicU64,
}

fn count(counter: &std::sync::atomic::AtomicU64, n: u64) {
    counter.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
}

const IDEMPOTENT_ATTEMPTS: usize = 4;

impl Storage {
    pub fn new(backend: Arc<dyn Backend>, prefix: &str) -> Self {
        let mut prefix = prefix.trim_matches('/').to_string();
        if !prefix.is_empty() {
            prefix.push('/');
        }
        Self {
            backend,
            prefix,
            verify_uploads: true,
            uploads: None,
            stats: Default::default(),
        }
    }

    pub fn stats(&self) -> &StorageStats {
        &self.stats
    }

    /// At most `n` pack uploads run at the same time.
    pub fn with_upload_limit(mut self, n: usize) -> Self {
        self.uploads = Some(Arc::new(tokio::sync::Semaphore::new(n.max(1))));
        self
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Whether a stored pack is read back and its checksum compared.
    pub fn with_upload_verification(mut self, v: bool) -> Self {
        self.verify_uploads = v;
        self
    }

    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    pub fn repo_prefix(&self, repo: RepoId) -> String {
        format!("{}repos/{repo}/", self.prefix)
    }
    pub fn head_key(&self, repo: RepoId) -> String {
        format!("{}head.json", self.repo_prefix(repo))
    }
    pub fn entry_key(&self, repo: RepoId, id: EntryId) -> String {
        format!("{}entries/{id}.json", self.repo_prefix(repo))
    }
    pub fn pack_key(&self, repo: RepoId, sha256: &str) -> String {
        format!("{}packs/{sha256}.pack", self.repo_prefix(repo))
    }
    pub fn snapshot_key(&self, repo: RepoId, id: SnapshotId) -> String {
        format!("{}snapshots/{id}.json", self.repo_prefix(repo))
    }
    pub fn receipt_key(&self, repo: RepoId, id: RequestId) -> String {
        format!("{}receipts/{id}.json", self.repo_prefix(repo))
    }

    /// Reads the head. With `known`, the service can answer "unchanged".
    pub async fn read_head(
        &self,
        repo: RepoId,
        known: Option<&ETag>,
    ) -> Result<HeadRead, StorageError> {
        let key = self.head_key(repo);
        Ok(match self.read_versioned::<Head>(&key, known).await? {
            Versioned::Missing => HeadRead::Missing,
            Versioned::Unchanged => HeadRead::Unchanged,
            Versioned::Found(head, etag) => {
                head.check(repo)
                    .map_err(|e| StorageError::Corrupt(format!("{key}: {e}")))?;
                HeadRead::Found { head, etag }
            }
        })
    }

    /// Reads an object that changes in place, with its ETag.
    pub async fn read_versioned<T: DeserializeOwned>(
        &self,
        key: &str,
        known: Option<&ETag>,
    ) -> Result<Versioned<T>, StorageError> {
        let mut last = String::new();
        for attempt in 0..IDEMPOTENT_ATTEMPTS {
            match self.backend.get(key, known.map(|e| e.0.as_str())).await {
                Ok(f) => {
                    let value: T = decode(key, &f.data)?;
                    let etag = f
                        .etag
                        .ok_or_else(|| StorageError::Other(format!("{key}: no ETag")))?;
                    return Ok(Versioned::Found(value, ETag(etag)));
                }
                Err(BackendError::NotFound) => return Ok(Versioned::Missing),
                Err(BackendError::NotModified) => return Ok(Versioned::Unchanged),
                Err(BackendError::Unavailable(m)) => {
                    last = m;
                    backoff(attempt).await;
                }
                Err(e) => return Err(StorageError::from_backend(key, e)),
            }
        }
        Err(StorageError::Unavailable(last))
    }

    /// Reads the head without a conditional request.
    pub async fn load_head(&self, repo: RepoId) -> Result<Option<(Head, ETag)>, StorageError> {
        match self.read_head(repo, None).await? {
            HeadRead::Found { head, etag } => Ok(Some((head, etag))),
            HeadRead::Missing => Ok(None),
            HeadRead::Unchanged => Err(StorageError::Other("unexpected not-modified".into())),
        }
    }

    /// Creates the head only if no head exists.
    pub async fn create_head(&self, head: &Head) -> CasOutcome {
        self.write_head(head, WriteMode::Create).await
    }

    /// Replaces the head only if its ETag equals `expected`. Exactly one
    /// request is sent; the caller resolves unknown results.
    pub async fn replace_head(&self, head: &Head, expected: &ETag) -> CasOutcome {
        self.write_head(head, WriteMode::Update(expected.0.clone()))
            .await
    }

    async fn write_head(&self, head: &Head, mode: WriteMode) -> CasOutcome {
        let key = self.head_key(head.repository);
        let revision = head.revision;
        self.write_versioned(&key, head, mode, |h: &Head| h.revision == revision)
            .await
    }

    /// Creates (`WriteMode::Create`) or replaces (`WriteMode::Update`) an
    /// object that changes in place. Exactly one request is sent; the caller
    /// resolves unknown results. `is_ours` recognizes the written value when
    /// the service accepts a write without returning an ETag.
    pub async fn write_versioned<T: Serialize + DeserializeOwned>(
        &self,
        key: &str,
        value: &T,
        mode: WriteMode,
        is_ours: impl Fn(&T) -> bool,
    ) -> CasOutcome {
        let data = Bytes::from(serde_json::to_vec_pretty(value).expect("record serializes"));
        match self.backend.put(key, data, mode).await {
            Ok(Some(etag)) => CasOutcome::Accepted(ETag(etag)),
            Ok(None) => match self.read_versioned::<T>(key, None).await {
                Ok(Versioned::Found(v, etag)) if is_ours(&v) => CasOutcome::Accepted(etag),
                Ok(_) => CasOutcome::Unknown("accepted write without ETag, since changed".into()),
                Err(e) => CasOutcome::Unknown(e.to_string()),
            },
            Err(BackendError::Conflict) => CasOutcome::Conflict,
            Err(e) => CasOutcome::Unknown(e.to_string()),
        }
    }

    /// Stores an immutable record. Repeating the call with identical contents
    /// succeeds; a different existing record is an error.
    pub async fn put_record<T: Serialize>(
        &self,
        key: &str,
        record: &T,
    ) -> Result<(), StorageError> {
        let data = Bytes::from(serde_json::to_vec_pretty(record).expect("record serializes"));
        let mut last = String::new();
        for attempt in 0..IDEMPOTENT_ATTEMPTS {
            match self.backend.put(key, data.clone(), WriteMode::Create).await {
                Ok(_) => return Ok(()),
                Err(BackendError::Conflict) => {
                    let existing = self
                        .backend
                        .get(key, None)
                        .await
                        .map_err(|e| StorageError::from_backend(key, e))?;
                    return if existing.data == data {
                        Ok(())
                    } else {
                        Err(StorageError::Corrupt(format!(
                            "{key} exists with different contents"
                        )))
                    };
                }
                Err(BackendError::Unavailable(m)) => {
                    last = m;
                    backoff(attempt).await;
                }
                Err(e) => return Err(StorageError::from_backend(key, e)),
            }
        }
        Err(StorageError::Unavailable(last))
    }

    pub async fn get_record<T: DeserializeOwned>(&self, key: &str) -> Result<T, StorageError> {
        let mut last = String::new();
        for attempt in 0..IDEMPOTENT_ATTEMPTS {
            match self.backend.get(key, None).await {
                Ok(f) => return decode(key, &f.data),
                Err(BackendError::Unavailable(m)) => {
                    last = m;
                    backoff(attempt).await;
                }
                Err(e) => return Err(StorageError::from_backend(key, e)),
            }
        }
        Err(StorageError::Unavailable(last))
    }

    pub async fn put_entry(&self, entry: &LogEntry) -> Result<(), StorageError> {
        self.put_record(&self.entry_key(entry.repository, entry.id), entry)
            .await
    }

    pub async fn get_entry(&self, repo: RepoId, id: EntryId) -> Result<LogEntry, StorageError> {
        let e: LogEntry = self.get_record(&self.entry_key(repo, id)).await?;
        if e.format != crate::model::FORMAT_VERSION {
            return Err(StorageError::Corrupt(format!(
                "entry {id}: unsupported format {}",
                e.format
            )));
        }
        if e.id != id || e.repository != repo {
            return Err(StorageError::Corrupt(format!(
                "entry {id}: identity mismatch"
            )));
        }
        Ok(e)
    }

    pub async fn put_snapshot(&self, s: &Snapshot) -> Result<(), StorageError> {
        self.put_record(&self.snapshot_key(s.repository, s.id), s)
            .await
    }

    pub async fn get_snapshot(
        &self,
        repo: RepoId,
        id: SnapshotId,
    ) -> Result<Snapshot, StorageError> {
        let s: Snapshot = self.get_record(&self.snapshot_key(repo, id)).await?;
        if s.format != crate::model::FORMAT_VERSION || s.id != id || s.repository != repo {
            return Err(StorageError::Corrupt(format!(
                "snapshot {id}: unsupported or mismatched record"
            )));
        }
        Ok(s)
    }

    pub async fn put_receipt(&self, r: &Receipt) -> Result<(), StorageError> {
        self.put_record(&self.receipt_key(r.repository, r.request), r)
            .await
    }

    pub async fn get_receipt(
        &self,
        repo: RepoId,
        id: RequestId,
    ) -> Result<Option<Receipt>, StorageError> {
        match self.get_record(&self.receipt_key(repo, id)).await {
            Ok(r) => Ok(Some(r)),
            Err(StorageError::Missing(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Uploads a pack file and checks upload completion and its checksum.
    pub async fn store_pack(&self, repo: RepoId, path: &Path) -> Result<PackRef, StorageError> {
        let (sha256, size) = file_sha256(path)
            .await
            .map_err(|e| StorageError::Other(e.to_string()))?;
        let key = self.pack_key(repo, &sha256);
        let pack = PackRef { sha256, size };
        let _permit = match &self.uploads {
            Some(s) => Some(s.clone().acquire_owned().await.expect("never closed")),
            None => None,
        };
        let mut last = String::new();
        for attempt in 0..IDEMPOTENT_ATTEMPTS {
            let result = async {
                // Always upload, even if the key exists: the new modification
                // time keeps cleanup from treating the pack as obsolete.
                self.backend.put_file(&key, path).await?;
                let stored = self.backend.size(&key).await?;
                if stored != size {
                    return Err(BackendError::Other(format!(
                        "stored size {stored} != {size}"
                    )));
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    count(&self.stats.pack_uploads, 1);
                    count(&self.stats.uploaded_bytes, size);
                    if self.verify_uploads {
                        self.verify_pack(repo, &pack).await?;
                    }
                    return Ok(pack);
                }
                Err(BackendError::Unavailable(m)) => {
                    last = m;
                    backoff(attempt).await;
                }
                Err(e) => return Err(StorageError::from_backend(&key, e)),
            }
        }
        Err(StorageError::Unavailable(last))
    }

    /// Reads a stored pack and compares its checksum and size.
    pub async fn verify_pack(&self, repo: RepoId, pack: &PackRef) -> Result<(), StorageError> {
        let key = self.pack_key(repo, &pack.sha256);
        let mut s = self
            .backend
            .get_stream(&key)
            .await
            .map_err(|e| StorageError::from_backend(&key, e))?;
        let mut h = Sha256::new();
        let mut n = 0u64;
        while let Some(chunk) = s.next().await {
            let chunk = chunk.map_err(|e| StorageError::from_backend(&key, e))?;
            n += chunk.len() as u64;
            h.update(&chunk);
        }
        let got = hex::encode(h.finalize());
        if got != pack.sha256 || n != pack.size {
            return Err(StorageError::Corrupt(format!(
                "{key}: checksum or size mismatch"
            )));
        }
        Ok(())
    }

    /// Downloads a pack to `dest` with limited memory and checks it.
    pub async fn fetch_pack(
        &self,
        repo: RepoId,
        pack: &PackRef,
        dest: &Path,
    ) -> Result<(), StorageError> {
        let key = self.pack_key(repo, &pack.sha256);
        let mut last = String::new();
        for attempt in 0..IDEMPOTENT_ATTEMPTS {
            match self.fetch_once(&key, pack, dest).await {
                Ok(()) => {
                    count(&self.stats.pack_downloads, 1);
                    count(&self.stats.downloaded_bytes, pack.size);
                    return Ok(());
                }
                Err(StorageError::Unavailable(m)) => {
                    last = m;
                    backoff(attempt).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(StorageError::Unavailable(last))
    }

    async fn fetch_once(&self, key: &str, pack: &PackRef, dest: &Path) -> Result<(), StorageError> {
        let io = |e: std::io::Error| StorageError::Other(format!("{}: {e}", dest.display()));
        let mut s = self
            .backend
            .get_stream(key)
            .await
            .map_err(|e| StorageError::from_backend(key, e))?;
        let mut f = tokio::fs::File::create(dest).await.map_err(io)?;
        let mut h = Sha256::new();
        let mut n = 0u64;
        while let Some(chunk) = s.next().await {
            let chunk = chunk.map_err(|e| StorageError::from_backend(key, e))?;
            n += chunk.len() as u64;
            h.update(&chunk);
            f.write_all(&chunk).await.map_err(io)?;
        }
        f.flush().await.map_err(io)?;
        let got = hex::encode(h.finalize());
        if got != pack.sha256 || n != pack.size {
            return Err(StorageError::Corrupt(format!(
                "{key}: checksum or size mismatch"
            )));
        }
        Ok(())
    }

    pub async fn list_repo(&self, repo: RepoId) -> Result<Vec<ObjectInfo>, StorageError> {
        let p = self.repo_prefix(repo);
        self.backend
            .list(p.trim_end_matches('/'))
            .await
            .map_err(|e| StorageError::from_backend(&p, e))
    }

    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        match self.backend.delete(key).await {
            Ok(()) | Err(BackendError::NotFound) => Ok(()),
            Err(e) => Err(StorageError::from_backend(key, e)),
        }
    }

    /// Reads raw bytes of any stored object (backup and tests).
    pub async fn get_raw(&self, key: &str) -> Result<Bytes, StorageError> {
        self.backend
            .get(key, None)
            .await
            .map(|f| f.data)
            .map_err(|e| StorageError::from_backend(key, e))
    }

    /// Writes raw bytes of an immutable object (backup restore).
    pub async fn put_raw_create(&self, key: &str, data: Bytes) -> Result<(), StorageError> {
        match self.backend.put(key, data.clone(), WriteMode::Create).await {
            Ok(_) => Ok(()),
            Err(BackendError::Conflict) => {
                let existing = self.get_raw(key).await?;
                if existing == data {
                    Ok(())
                } else {
                    Err(StorageError::Corrupt(format!("{key} differs")))
                }
            }
            Err(e) => Err(StorageError::from_backend(key, e)),
        }
    }
}

fn decode<T: DeserializeOwned>(key: &str, data: &[u8]) -> Result<T, StorageError> {
    serde_json::from_slice(data).map_err(|e| StorageError::Corrupt(format!("{key}: {e}")))
}

async fn backoff(attempt: usize) {
    let base = 50u64 << attempt.min(5);
    let jitter = rand::random::<u64>() % base.max(1);
    tokio::time::sleep(Duration::from_millis(base + jitter)).await;
}

/// SHA-256 and byte count of a file, read with a fixed buffer.
pub async fn file_sha256(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut n = 0u64;
    loop {
        let r = f.read(&mut buf).await?;
        if r == 0 {
            break;
        }
        n += r as u64;
        h.update(&buf[..r]);
    }
    Ok((hex::encode(h.finalize()), n))
}
