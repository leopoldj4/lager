//! Snapshots and compaction.
//!
//! A snapshot is also the compaction step: one combined pack holds every
//! object reachable from the refs of the selected generation. Compaction
//! never changes refs and never removes an accepted push: the snapshot is
//! attached by a maintenance entry after the current latest entry, and all
//! entries after its base remain part of recovery. Attaching it removes the
//! entries up to its base from the head's index.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lager_core::cas::{Resolution, resolve_unknown_cas};
use lager_core::model::{
    EntryId, FORMAT_VERSION, LOG_FORMAT_VERSION, LogEntry, Oid, Operation, RepoState, RequestId,
    Snapshot, SnapshotId, now_ms, request_checksum,
};
use lager_core::storage::CasOutcome;
use serde::Serialize;

use crate::metrics::StepTimer;
use crate::repository::push::{Fence, fence_check};
use crate::repository::{Controller, Registry, RepoError};

#[derive(Debug, Serialize)]
pub struct SnapshotReport {
    pub snapshot: SnapshotId,
    pub base_generation: u64,
    /// Generation of the maintenance entry that attached the snapshot.
    pub generation: u64,
    pub objects: u32,
    pub attempts: u32,
}

/// Creates a snapshot of the current generation and attaches it with CAS.
pub async fn snapshot(c: &Controller, user: &str) -> Result<SnapshotReport, RepoError> {
    let svc = &c.svc;
    let _t = StepTimer::new(&svc.metrics, "snapshot");
    // One local write operation at a time; other servers may still push.
    let _w = c.write_op.lock().await;

    // 1-2. Select an accepted generation and protect its files.
    let (known, cache) = {
        let st = c.sync_to_storage().await?;
        let k = st
            .known
            .clone()
            .ok_or_else(|| RepoError::Internal("cache not ready".into()))?;
        let cache = st
            .cache
            .clone()
            .ok_or_else(|| RepoError::Internal("cache not ready".into()))?;
        drop(st);
        (k, cache)
    };

    // 3-5. Combined pack and snapshot record, checked and uploaded.
    let tips: Vec<Oid> = known.refs.refs.values().cloned().collect();
    let mut packs = Vec::new();
    let mut objects = 0;
    // Kept until the snapshot is attached, in case a removal fence requires
    // another upload.
    let file = svc
        .tmp_dir()
        .join(format!("snapshot-{}.pack", uuid::Uuid::new_v4().simple()));
    let _remove = RemoveOnDrop(file.clone());
    let mut uploaded_at = now_ms();
    if !tips.is_empty() {
        objects = svc.git.pack_objects(&cache.dir, &tips, &[], &file).await?;
        packs.push(svc.storage.store_pack(c.repo, &file).await?);
        uploaded_at = now_ms();
    }
    let snap = Snapshot {
        format: FORMAT_VERSION,
        repository: c.repo,
        incarnation: known.head.incarnation,
        id: SnapshotId::new(),
        base_entry: known.entry,
        base_generation: known.head.generation,
        object_format: known.head.object_format,
        head: known
            .refs
            .head
            .clone()
            .ok_or_else(|| RepoError::Internal("no default branch".into()))?,
        refs: known.refs.refs.clone(),
        packs,
    };
    svc.storage.put_snapshot(&snap).await?;

    // 6-7. Maintenance entry after the current latest entry, attached with CAS.
    let request = RequestId::new();
    for attempt in 1..=lager_core::limits::PUSH_RETRY_LIMIT * 2 {
        let (head, etag) = svc
            .storage
            .load_head(c.repo)
            .await?
            .ok_or(RepoError::NotFound)?;
        if head.state == RepoState::Deleted {
            return Err(RepoError::Deleted);
        }
        if head.incarnation != snap.incarnation {
            return Err(RepoError::Internal("repository was recreated".into()));
        }
        match fence_check(&svc.storage, &head, &snap.packs, uploaded_at).await? {
            Fence::Clear => {}
            Fence::Reupload => {
                svc.storage.store_pack(c.repo, &file).await?;
                uploaded_at = now_ms();
                continue;
            }
            Fence::Wait => {
                return Err(RepoError::Unavailable(
                    "stored data removal in progress; retry the snapshot later".into(),
                ));
            }
        }
        // The index must still reach the snapshot base: a newer snapshot
        // already folded the entries after it.
        if head
            .snapshot
            .as_ref()
            .is_some_and(|s| s.base_generation > snap.base_generation)
        {
            return Err(RepoError::Internal(
                "a newer snapshot is already attached".into(),
            ));
        }
        if !head.contains(snap.base_generation, snap.base_entry) {
            return Err(RepoError::Internal(
                "snapshot base is no longer in the history".into(),
            ));
        }
        let entry = LogEntry {
            format: LOG_FORMAT_VERSION,
            repository: c.repo,
            incarnation: head.incarnation,
            id: EntryId::new(),
            generation: head.generation + 1,
            request,
            request_checksum: request_checksum(user, &[]),
            operation: Operation::Snapshot {
                snapshot: snap.id,
                base_entry: snap.base_entry,
                base_generation: snap.base_generation,
            },
            refs: vec![],
            packs: vec![],
            user: user.into(),
            rules_version: 0,
            received_at_ms: now_ms(),
        };
        svc.storage.put_entry(&entry).await?;
        let next = head.advance(&entry);
        let accepted = match svc.storage.replace_head(&next, &etag).await {
            CasOutcome::Accepted(_) => true,
            CasOutcome::Conflict => false,
            CasOutcome::Unknown(_) => {
                let until = Instant::now() + lager_core::limits::UNKNOWN_RESULT_DEADLINE;
                match resolve_unknown_cas(&svc.storage, &next, &etag, entry.id, until).await {
                    Resolution::Accepted(_) => true,
                    Resolution::Rejected => false,
                    Resolution::Unknown(m) => {
                        return Err(RepoError::Unavailable(format!(
                            "snapshot result unknown: {m}"
                        )));
                    }
                }
            }
        };
        if accepted {
            svc.metrics.inc("snapshots");
            return Ok(SnapshotReport {
                snapshot: snap.id,
                base_generation: snap.base_generation,
                generation: entry.generation,
                objects,
                attempts: attempt,
            });
        }
        svc.metrics.inc("snapshot_conflicts");
        tokio::time::sleep(Duration::from_millis(20 << attempt.min(6))).await;
    }
    Err(RepoError::Internal(
        "snapshot not attached: too many concurrent changes".into(),
    ))
}

struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Creates snapshots for repositories whose history since the last snapshot
/// reached `after_entries`. Only repositories that this server accepted a
/// push for are handled, so that usually one server does this work; when
/// two do, CAS keeps one snapshot. Returns the number of snapshots.
pub async fn auto_snapshot(registry: &Arc<Registry>, after_entries: u64) -> usize {
    let mut n = 0;
    for c in registry.controllers() {
        if !c.pushed_here.load(Ordering::SeqCst) {
            continue;
        }
        let Some(head) = c.cached_head().await else {
            continue;
        };
        let since = head.generation - head.snapshot.as_ref().map_or(0, |s| s.base_generation);
        if head.state != RepoState::Active || since < after_entries {
            c.pushed_here.store(false, Ordering::SeqCst);
            continue;
        }
        match snapshot(&c, "lager-maintenance").await {
            Ok(r) => {
                n += 1;
                c.pushed_here.store(false, Ordering::SeqCst);
                tracing::info!(repo = %c.repo, base = r.base_generation, "automatic snapshot");
            }
            Err(e) => tracing::warn!(repo = %c.repo, error = %e, "automatic snapshot failed"),
        }
    }
    n
}
