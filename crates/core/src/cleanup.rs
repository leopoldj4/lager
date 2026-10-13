//! Safe removal of obsolete stored data.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cas::{Resolution, resolve_unknown_cas};
use crate::history::load_entries;
use crate::model::{
    EntryId, Head, LOG_FORMAT_VERSION, LogEntry, Operation, RepoId, RepoState, RequestId, now_ms,
    request_checksum,
};
use crate::storage::{CasOutcome, ETag, Storage, StorageError};

#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    #[error("repository not found")]
    NotFound,
    #[error("storage unavailable: {0}")]
    Unavailable(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Default allowed clock difference between servers for removal fences.
pub const CLOCK_MARGIN: Duration = Duration::from_secs(30);

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
    pub fn minimum_retention() -> Duration {
        crate::limits::PUSH_DEADLINE + crate::limits::UNKNOWN_RESULT_DEADLINE + CLOCK_MARGIN
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
///   fence expires (see the server's `fence_check`);
/// - removals stop when the fence expires.
pub async fn cleanup(
    storage: &Storage,
    repo: RepoId,
    opts: &CleanupOptions,
) -> Result<CleanupReport, CleanupError> {
    for _ in 0..8 {
        let (head, etag) = storage
            .load_head(repo)
            .await?
            .ok_or(CleanupError::NotFound)?;
        if let Some(r) = &head.removal
            && !opts.report_only
            && now_ms() < r.reuse_after_ms
        {
            return Err(CleanupError::Unavailable(
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
    Err(CleanupError::Unavailable(
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
) -> Result<CleanupReport, CleanupError> {
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
) -> Result<Option<u64>, CleanupError> {
    let entry = LogEntry {
        format: LOG_FORMAT_VERSION,
        repository: head.repository,
        incarnation: head.incarnation,
        id: EntryId::new(),
        generation: head.generation + 1,
        request: RequestId::new(),
        request_checksum: request_checksum("worktree-cleanup", &[]),
        operation: Operation::Removal {
            packs,
            expires_at_ms,
            reuse_after_ms,
        },
        refs: vec![],
        packs: vec![],
        user: "worktree-cleanup".into(),
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
                Resolution::Unknown(m) => Err(CleanupError::Unavailable(format!(
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
) -> Result<(), CleanupError> {
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
