//! Snapshots, compaction, and safe removal of obsolete stored data
//!.
//!
//! A snapshot is also the compaction step: one combined pack holds every
//! object reachable from the refs of the selected generation. Compaction
//! never changes refs and never removes an accepted push: the snapshot is
//! attached by a maintenance entry after the current latest entry, and all
//! entries after its base remain part of recovery.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::metrics::StepTimer;
use crate::model::{
    EntryId, FORMAT_VERSION, LogEntry, Oid, Operation, RepoId, RepoState, RequestId, Snapshot,
    SnapshotId, now_ms, request_checksum,
};
use crate::repository::push::{Resolution, resolve_unknown_cas};
use crate::repository::sync::load_entries;
use crate::repository::{Controller, RepoError};
use crate::storage::{CasOutcome, Storage, StorageError};

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
    if !tips.is_empty() {
        let file = svc
            .tmp_dir()
            .join(format!("snapshot-{}.pack", uuid::Uuid::new_v4().simple()));
        let r = async {
            objects = svc.git.pack_objects(&cache.dir, &tips, &[], &file).await?;
            Ok::<_, RepoError>(svc.storage.store_pack(c.repo, &file).await?)
        }
        .await;
        let _ = tokio::fs::remove_file(&file).await;
        packs.push(r?);
    }
    let snap = Snapshot {
        format: FORMAT_VERSION,
        repository: c.repo,
        incarnation: known.head.incarnation,
        id: SnapshotId::new(),
        base_entry: known.head.latest_entry,
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
    for attempt in 1..=svc.limits.push_retry_limit.max(1) * 2 {
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
        // The snapshot base must remain in the history of the new head.
        let back = load_entries(&svc.storage, &head, Some(snap.base_generation)).await?;
        if back.first().map(|e| e.id) != Some(snap.base_entry) {
            return Err(RepoError::Internal(
                "snapshot base is no longer in the history".into(),
            ));
        }
        let entry = LogEntry {
            format: FORMAT_VERSION,
            repository: c.repo,
            incarnation: head.incarnation,
            id: EntryId::new(),
            previous: Some(head.latest_entry),
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
                let until =
                    Instant::now() + Duration::from_secs(svc.limits.unknown_result_deadline_secs);
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

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RemovalCandidate {
    pub key: String,
    pub size: u64,
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CleanupReport {
    pub repository: RepoId,
    pub report_only: bool,
    /// Stored objects that recovery or request results need.
    pub required: usize,
    /// Obsolete, but younger than the retention period.
    pub retained_recent: usize,
    pub candidates: Vec<RemovalCandidate>,
    pub removed: usize,
}

/// Finds stored data that neither recovery nor request results need.
///
/// Required (never removed):
/// - the head, every entry in the history, and every receipt;
/// - the snapshot the head points to and its packs;
/// - the packs of every entry after the snapshot base (all entries without a snapshot).
///
/// Everything else is obsolete: superseded snapshots, packs that only
/// entries at or before the snapshot base use, and uploads of attempts that
/// never became visible. Obsolete data younger than `retention` is kept: it
/// can belong to an active push or a request with an unknown result, whose
/// deadlines are much shorter than the retention period. Each candidate's
/// age is checked again immediately before removal.
pub async fn cleanup(
    storage: &Storage,
    repo: RepoId,
    retention: Duration,
    report_only: bool,
) -> Result<CleanupReport, StorageError> {
    let (head, _) = storage
        .load_head(repo)
        .await?
        .ok_or_else(|| StorageError::Missing(storage.head_key(repo)))?;
    let mut required: HashSet<String> = HashSet::new();
    required.insert(storage.head_key(repo));

    // The complete history, ignoring the snapshot shortcut.
    let mut history = Vec::new();
    let mut next = Some(head.latest_entry);
    while let Some(id) = next {
        let e = storage.get_entry(repo, id).await?;
        next = e.previous;
        history.push(e);
    }
    let base_generation = head.snapshot.as_ref().map(|s| s.base_generation);
    for e in &history {
        required.insert(storage.entry_key(repo, e.id));
        if head.state == RepoState::Active && base_generation.is_none_or(|g| e.generation > g) {
            for p in &e.packs {
                required.insert(storage.pack_key(repo, &p.sha256));
            }
        }
    }
    if let (Some(p), RepoState::Active) = (&head.snapshot, head.state) {
        let s = storage.get_snapshot(repo, p.id).await?;
        required.insert(storage.snapshot_key(repo, p.id));
        for pk in &s.packs {
            required.insert(storage.pack_key(repo, &pk.sha256));
        }
    }

    let now = now_ms();
    let retention_ms = retention.as_millis() as u64;
    let mut report = CleanupReport {
        repository: repo,
        report_only,
        required: 0,
        retained_recent: 0,
        candidates: vec![],
        removed: 0,
    };
    let receipts = format!("{}receipts/", storage.repo_prefix(repo));
    for obj in storage.list_repo(repo).await? {
        if required.contains(&obj.key) || obj.key.starts_with(&receipts) {
            report.required += 1;
            continue;
        }
        if now.saturating_sub(obj.last_modified_ms) < retention_ms {
            report.retained_recent += 1;
            continue;
        }
        let reason = if obj.key.contains("/packs/") {
            "pack not needed by the snapshot or later entries"
        } else if obj.key.contains("/snapshots/") {
            "superseded snapshot"
        } else if obj.key.contains("/entries/") {
            "entry of an attempt that never became visible"
        } else {
            "unknown object"
        };
        report.candidates.push(RemovalCandidate {
            key: obj.key,
            size: obj.size,
            reason,
        });
    }

    if !report_only {
        // Check each candidate again right before removal.
        let fresh: std::collections::HashMap<String, u64> = storage
            .list_repo(repo)
            .await?
            .into_iter()
            .map(|o| (o.key, o.last_modified_ms))
            .collect();
        for c in &report.candidates {
            match fresh.get(&c.key) {
                Some(t) if now_ms().saturating_sub(*t) >= retention_ms => {
                    storage.delete(&c.key).await?;
                    report.removed += 1;
                }
                _ => {}
            }
        }
    }
    Ok(report)
}
