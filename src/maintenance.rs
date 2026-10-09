//! Snapshots, compaction, and safe removal of obsolete stored data
//!.
//!
//! A snapshot is also the compaction step: one combined pack holds every
//! object reachable from the refs of the selected generation. Compaction
//! never changes refs and never removes an accepted push: the snapshot is
//! attached by a maintenance entry after the current latest entry, and all
//! entries after its base remain part of recovery. Attaching it removes the
//! entries up to its base from the head's index.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::Limits;
use crate::metrics::StepTimer;
use crate::model::{
    EntryId, FORMAT_VERSION, Head, LOG_FORMAT_VERSION, LogEntry, Oid, Operation, RepoId, RepoState,
    RequestId, Snapshot, SnapshotId, now_ms, request_checksum,
};
use crate::repository::push::{Fence, Resolution, fence_check, resolve_unknown_cas};
use crate::repository::sync::load_entries;
use crate::repository::{Controller, Registry, RepoError};
use crate::storage::{CasOutcome, ETag, Storage};

/// Default allowed clock difference between servers for removal fences.
pub const CLOCK_MARGIN: Duration = Duration::from_secs(30);

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

struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Creates snapshots for repositories whose history since the last snapshot
/// reached `after_entries`. Only repositories for which `preferred` is true
/// are handled, so that one server does this work. Returns the number of
/// snapshots.
pub async fn auto_snapshot(
    registry: &Arc<Registry>,
    after_entries: u64,
    preferred: impl Fn(RepoId) -> bool,
) -> usize {
    if after_entries == 0 {
        return 0;
    }
    let mut n = 0;
    for c in registry.controllers() {
        let Some(head) = c.cached_head().await else {
            continue;
        };
        let since = head.generation - head.snapshot.as_ref().map_or(0, |s| s.base_generation);
        if head.state != RepoState::Active || since < after_entries || !preferred(c.repo) {
            continue;
        }
        match snapshot(&c, "rustgit-maintenance").await {
            Ok(r) => {
                n += 1;
                tracing::info!(repo = %c.repo, base = r.base_generation, "automatic snapshot");
            }
            Err(e) => tracing::warn!(repo = %c.repo, error = %e, "automatic snapshot failed"),
        }
    }
    n
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RemovalCandidate {
    pub key: String,
    pub size: u64,
    pub reason: &'static str,
}

/// Retention periods and mode of one data removal run.
#[derive(Clone, Debug)]
pub struct CleanupOptions {
    /// Obsolete packs (recoverable contents) younger than this are kept.
    pub pack_retention: Duration,
    /// Receipts, entries of attempts that never became visible, and
    /// superseded snapshot records younger than this are kept.
    pub record_retention: Duration,
    /// Removals must finish this long after the fence.
    pub removal_window: Duration,
    /// Allowed clock difference between servers: changes reuse listed
    /// packs only this long after the removal window.
    pub clock_margin: Duration,
    pub report_only: bool,
}

impl CleanupOptions {
    pub fn report(retention: Duration) -> Self {
        Self {
            pack_retention: retention,
            record_retention: retention,
            removal_window: Duration::from_secs(600),
            clock_margin: CLOCK_MARGIN,
            report_only: true,
        }
    }

    pub fn remove(retention: Duration) -> Self {
        Self {
            report_only: false,
            ..Self::report(retention)
        }
    }

    /// The shortest retention period that protects active pushes and
    /// requests with unknown results: their uploads are younger than this.
    pub fn minimum_retention(limits: &Limits) -> Duration {
        Duration::from_secs(limits.push_deadline_secs + limits.unknown_result_deadline_secs)
            + CLOCK_MARGIN
    }
}

#[derive(Debug, Serialize)]
pub struct CleanupReport {
    pub repository: RepoId,
    pub report_only: bool,
    /// Stored objects that recovery needs.
    pub required: usize,
    /// Obsolete, but younger than the retention period.
    pub retained_recent: usize,
    pub candidates: Vec<RemovalCandidate>,
    /// Generation of the fence entry that preceded the removals.
    pub fence_generation: Option<u64>,
    pub removed: usize,
    /// The removal window ended before all candidates were removed.
    pub incomplete: bool,
}

/// Finds stored data that neither recovery nor request results need, and
/// removes it unless `report_only`.
///
/// Required (never removed):
/// - the head and every entry in its index;
/// - the entry of the latest removal fence;
/// - the snapshot the head points to and its packs;
/// - the packs of every entry in the index.
///
/// Everything else is obsolete: superseded snapshots, entries and packs
/// that a snapshot folded, uploads of attempts that never became visible,
/// and receipts. Obsolete data younger than its retention period is kept:
/// active pushes and requests with unknown results use only younger uploads.
///
/// Removal protects operations with a fence. Before the first removal, a
/// `Removal` entry that lists the packs to remove is attached with CAS:
/// - every pending CAS against an earlier head can no longer succeed, so an
///   expired operation cannot later accept a change that needs removed data;
/// - a later change that needs a listed pack uploads it again after the
///   fence expires (see `repository::push::fence_check`);
/// - removals stop when the fence expires.
pub async fn cleanup(
    storage: &Storage,
    repo: RepoId,
    opts: &CleanupOptions,
) -> Result<CleanupReport, RepoError> {
    for _ in 0..8 {
        let (head, etag) = storage.load_head(repo).await?.ok_or(RepoError::NotFound)?;
        if let Some(r) = &head.removal
            && !opts.report_only
            && now_ms() < r.reuse_after_ms
        {
            return Err(RepoError::Unavailable(
                "another data removal is in progress".into(),
            ));
        }
        let mut report = plan(storage, &head, opts).await?;
        if opts.report_only || report.candidates.is_empty() {
            return Ok(report);
        }
        let expires_at_ms = now_ms() + opts.removal_window.as_millis() as u64;
        let reuse_after_ms = expires_at_ms + opts.clock_margin.as_millis() as u64;
        let packs: Vec<String> = report
            .candidates
            .iter()
            .filter_map(|c| pack_sha(&c.key))
            .collect();
        let Some(generation) =
            attach_fence(storage, &head, &etag, packs, expires_at_ms, reuse_after_ms).await?
        else {
            continue;
        };
        report.fence_generation = Some(generation);
        remove(storage, repo, opts, expires_at_ms, &mut report).await?;
        return Ok(report);
    }
    Err(RepoError::Unavailable(
        "data removal not started: too many concurrent changes".into(),
    ))
}

fn pack_sha(key: &str) -> Option<String> {
    let name = key.rsplit_once("/packs/")?.1;
    name.strip_suffix(".pack").map(str::to_string)
}

async fn plan(
    storage: &Storage,
    head: &Head,
    opts: &CleanupOptions,
) -> Result<CleanupReport, RepoError> {
    let repo = head.repository;
    let mut required: HashSet<String> = HashSet::new();
    required.insert(storage.head_key(repo));

    for e in load_entries(storage, head, head.first_generation()).await? {
        required.insert(storage.entry_key(repo, e.id));
        if head.state == RepoState::Active {
            for p in &e.packs {
                required.insert(storage.pack_key(repo, &p.sha256));
            }
        }
    }
    // Pushes read the fence entry until a later fence replaces it.
    if let Some(r) = &head.removal {
        required.insert(storage.entry_key(repo, r.entry));
    }
    if let (Some(p), RepoState::Active) = (&head.snapshot, head.state) {
        let s = storage.get_snapshot(repo, p.id).await?;
        required.insert(storage.snapshot_key(repo, p.id));
        for pk in &s.packs {
            required.insert(storage.pack_key(repo, &pk.sha256));
        }
    }

    let now = now_ms();
    let mut report = CleanupReport {
        repository: repo,
        report_only: opts.report_only,
        required: 0,
        retained_recent: 0,
        candidates: vec![],
        fence_generation: None,
        removed: 0,
        incomplete: false,
    };
    for obj in storage.list_repo(repo).await? {
        if required.contains(&obj.key) {
            report.required += 1;
            continue;
        }
        let (retention, reason) = classify(&obj.key, opts);
        if now.saturating_sub(obj.last_modified_ms) < retention.as_millis() as u64 {
            report.retained_recent += 1;
            continue;
        }
        report.candidates.push(RemovalCandidate {
            key: obj.key,
            size: obj.size,
            reason,
        });
    }
    Ok(report)
}

fn classify(key: &str, opts: &CleanupOptions) -> (Duration, &'static str) {
    if key.contains("/packs/") {
        (
            opts.pack_retention,
            "pack not needed by the snapshot or the index",
        )
    } else if key.contains("/snapshots/") {
        (opts.record_retention, "superseded snapshot")
    } else if key.contains("/entries/") {
        (
            opts.record_retention,
            "entry not in the index: never accepted, or folded into a snapshot",
        )
    } else if key.contains("/receipts/") {
        (
            opts.record_retention,
            "receipt older than the retention period",
        )
    } else {
        (
            opts.pack_retention.max(opts.record_retention),
            "unknown object",
        )
    }
}

/// Attaches the fence entry. `None` when another change came first.
async fn attach_fence(
    storage: &Storage,
    head: &Head,
    etag: &ETag,
    packs: Vec<String>,
    expires_at_ms: u64,
    reuse_after_ms: u64,
) -> Result<Option<u64>, RepoError> {
    let entry = LogEntry {
        format: LOG_FORMAT_VERSION,
        repository: head.repository,
        incarnation: head.incarnation,
        id: EntryId::new(),
        generation: head.generation + 1,
        request: RequestId::new(),
        request_checksum: request_checksum("rustgit-cleanup", &[]),
        operation: Operation::Removal {
            packs,
            expires_at_ms,
            reuse_after_ms,
        },
        refs: vec![],
        packs: vec![],
        user: "rustgit-cleanup".into(),
        rules_version: 0,
        received_at_ms: now_ms(),
    };
    storage.put_entry(&entry).await?;
    let next = head.advance(&entry);
    match storage.replace_head(&next, etag).await {
        CasOutcome::Accepted(_) => Ok(Some(entry.generation)),
        CasOutcome::Conflict => Ok(None),
        CasOutcome::Unknown(_) => {
            let until = Instant::now() + Duration::from_secs(30);
            match resolve_unknown_cas(storage, &next, etag, entry.id, until).await {
                Resolution::Accepted(_) => Ok(Some(entry.generation)),
                Resolution::Rejected => Ok(None),
                // Nothing is removed while the fence is not known.
                Resolution::Unknown(m) => Err(RepoError::Unavailable(format!(
                    "removal fence result unknown: {m}"
                ))),
            }
        }
    }
}

async fn remove(
    storage: &Storage,
    repo: RepoId,
    opts: &CleanupOptions,
    expires_at_ms: u64,
    report: &mut CleanupReport,
) -> Result<(), RepoError> {
    // Check each candidate's age again right before removal.
    let fresh: std::collections::HashMap<String, u64> = storage
        .list_repo(repo)
        .await?
        .into_iter()
        .map(|o| (o.key, o.last_modified_ms))
        .collect();
    for c in &report.candidates {
        if now_ms() >= expires_at_ms {
            report.incomplete = true;
            break;
        }
        let (retention, _) = classify(&c.key, opts);
        match fresh.get(&c.key) {
            Some(t) if now_ms().saturating_sub(*t) >= retention.as_millis() as u64 => {
                storage.delete(&c.key).await?;
                report.removed += 1;
            }
            _ => {}
        }
    }
    Ok(())
}
