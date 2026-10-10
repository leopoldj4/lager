//! Backup and recovery commands.
//!
//! An export copies the snapshot, every entry in the head's index, the
//! receipts, and every pack that recovery needs to a local directory. An
//! import writes the immutable records first and creates the head last, with
//! a conditional request, so an import never overwrites an existing
//! repository.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::history::{chain_state, load_chain};
use crate::model::{Head, PackRef, RepoId};
use crate::storage::{Storage, StorageError};

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub repository: RepoId,
    pub generation: u64,
    pub records: Vec<String>,
    pub packs: Vec<PackRef>,
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid backup: {0}")]
    Invalid(String),
    #[error("the target storage already has this repository")]
    Exists,
}

fn rel(storage: &Storage, repo: RepoId, key: &str) -> String {
    key.strip_prefix(&storage.repo_prefix(repo))
        .unwrap_or(key)
        .to_string()
}

/// Exports one repository to `dir`.
pub async fn export(storage: &Storage, repo: RepoId, dir: &Path) -> Result<Manifest, BackupError> {
    let (head, _) = storage
        .load_head(repo)
        .await?
        .ok_or_else(|| BackupError::Invalid("no repository".into()))?;
    tokio::fs::create_dir_all(dir.join("entries")).await?;
    tokio::fs::create_dir_all(dir.join("packs")).await?;
    tokio::fs::create_dir_all(dir.join("snapshots")).await?;
    tokio::fs::create_dir_all(dir.join("receipts")).await?;
    let mut records = Vec::new();
    let mut packs: Vec<PackRef> = Vec::new();

    // The indexed history must be complete.
    let chain = load_chain(storage, &head).await?;
    chain_state(&chain).map_err(BackupError::Invalid)?;
    if let Some(s) = &chain.snapshot {
        let key = storage.snapshot_key(repo, s.id);
        tokio::fs::write(
            dir.join(rel(storage, repo, &key)),
            storage.get_raw(&key).await?,
        )
        .await?;
        records.push(rel(storage, repo, &key));
        packs.extend(s.packs.iter().cloned());
    }
    for e in &chain.entries {
        packs.extend(e.packs.iter().cloned());
        let key = storage.entry_key(repo, e.id);
        tokio::fs::write(
            dir.join(rel(storage, repo, &key)),
            storage.get_raw(&key).await?,
        )
        .await?;
        records.push(rel(storage, repo, &key));
    }
    // Pushes read the latest removal fence.
    if let Some(r) = &head.removal
        && !chain.entries.iter().any(|e| e.id == r.entry)
    {
        let key = storage.entry_key(repo, r.entry);
        tokio::fs::write(
            dir.join(rel(storage, repo, &key)),
            storage.get_raw(&key).await?,
        )
        .await?;
        records.push(rel(storage, repo, &key));
    }
    let receipts_prefix = format!("{}receipts/", storage.repo_prefix(repo));
    for obj in storage.list_repo(repo).await? {
        if obj.key.starts_with(&receipts_prefix) {
            tokio::fs::write(
                dir.join(rel(storage, repo, &obj.key)),
                storage.get_raw(&obj.key).await?,
            )
            .await?;
            records.push(rel(storage, repo, &obj.key));
        }
    }
    packs.sort_by(|a, b| a.sha256.cmp(&b.sha256));
    packs.dedup();
    for p in &packs {
        storage
            .fetch_pack(
                repo,
                p,
                &dir.join("packs").join(format!("{}.pack", p.sha256)),
            )
            .await?;
    }
    tokio::fs::write(
        dir.join("head.json"),
        serde_json::to_vec_pretty(&head).expect("serializes"),
    )
    .await?;
    let manifest = Manifest {
        repository: repo,
        generation: head.generation,
        records,
        packs,
    };
    tokio::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serializes"),
    )
    .await?;
    Ok(manifest)
}

/// Imports a repository exported by [`export`] into (possibly different) storage.
pub async fn import(storage: &Storage, dir: &Path) -> Result<Head, BackupError> {
    let manifest: Manifest =
        serde_json::from_slice(&tokio::fs::read(dir.join("manifest.json")).await?)
            .map_err(|e| BackupError::Invalid(e.to_string()))?;
    let head: Head = serde_json::from_slice(&tokio::fs::read(dir.join("head.json")).await?)
        .map_err(|e| BackupError::Invalid(e.to_string()))?;
    head.check(manifest.repository)
        .map_err(|e| BackupError::Invalid(e.to_string()))?;
    let repo = manifest.repository;
    if storage.load_head(repo).await?.is_some() {
        return Err(BackupError::Exists);
    }
    for p in &manifest.packs {
        let stored = storage
            .store_pack(repo, &dir.join("packs").join(format!("{}.pack", p.sha256)))
            .await?;
        if &stored != p {
            return Err(BackupError::Invalid(format!(
                "pack {} does not match the manifest",
                p.sha256
            )));
        }
    }
    for r in &manifest.records {
        if r.contains("..") || r.starts_with('/') {
            return Err(BackupError::Invalid(format!("invalid record path {r}")));
        }
        let data = tokio::fs::read(dir.join(r)).await?;
        storage
            .put_raw_create(&format!("{}{r}", storage.repo_prefix(repo)), data.into())
            .await?;
    }
    // The records must complete the index before the head makes them visible.
    let chain = load_chain(storage, &head).await?;
    chain_state(&chain).map_err(BackupError::Invalid)?;
    match storage.create_head(&head).await {
        crate::storage::CasOutcome::Accepted(_) => Ok(head),
        crate::storage::CasOutcome::Conflict => Err(BackupError::Exists),
        crate::storage::CasOutcome::Unknown(m) => {
            Err(BackupError::Storage(StorageError::Unavailable(m)))
        }
    }
}
