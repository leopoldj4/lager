//! Fixed resource limits and timings. They are not configurable.

use std::time::Duration;

/// Maximum bytes in one push request body.
pub const MAX_RECEIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum simultaneous Git processes for client requests.
pub const MAX_GIT_PROCESSES: usize = 64;
/// Reject work that would leave less free disk space than this.
pub const MIN_FREE_DISK_BYTES: u64 = 1024 * 1024 * 1024;
/// Disk space reserved for a push without a Content-Length.
pub const DEFAULT_PUSH_RESERVATION_BYTES: u64 = 256 * 1024 * 1024;
/// CAS attempts against newer heads before a retryable conflict.
pub const PUSH_RETRY_LIMIT: u32 = 8;
/// Time limit for one push procedure, after the objects are received.
pub const PUSH_DEADLINE: Duration = Duration::from_secs(120);
/// Time limit to resolve an unknown CAS result.
pub const UNKNOWN_RESULT_DEADLINE: Duration = Duration::from_secs(30);
/// Close idle client connections after this time.
pub const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Remove caches that were not used for this time.
pub const CACHE_IDLE: Duration = Duration::from_secs(24 * 3600);
/// Largest single Git object in a push, after decompression.
pub const MAX_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
/// Total size of the new objects in a push, after decompression.
pub const MAX_EXPANDED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// CPU seconds for one Git process of a client request. 0: no limit.
pub const GIT_CPU_SECS: u64 = 0;
/// Address space of one Git process of a client request (Linux). 0: no
/// limit. Git maps large packs into memory, so a limit breaks big
/// repositories.
pub const GIT_MEMORY_BYTES: u64 = 0;
/// Time a request waits for a free Git process before a busy response.
pub const PROCESS_WAIT: Duration = Duration::from_secs(10);
/// Simultaneous pack uploads to storage.
pub const MAX_CONCURRENT_UPLOADS: usize = 16;
/// Time limit for one storage request.
pub const STORAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// On stop, the health check fails for this long before the server stops
/// accepting connections, so that a load balancer can remove it first.
pub const DRAIN_DELAY: Duration = Duration::from_secs(5);
/// On stop, the longest wait for running requests and pushes.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(180);

/// A catalog lookup is reused for this time. Deleted repositories are never
/// served: each request also checks the repository head.
pub const CATALOG_CACHE: Duration = Duration::from_secs(5);

/// Create a snapshot when this many entries follow the last snapshot. The
/// head's index lists those entries, so snapshots keep it short.
pub const SNAPSHOT_AFTER_ENTRIES: u64 = 200;
/// How often the server looks for repositories that need a snapshot.
pub const SNAPSHOT_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// Database connections of one process.
pub const DATABASE_CONNECTIONS: u32 = 8;
/// Longest wait for a database connection; the request then fails with 503.
pub const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);

/// First NATS subject token: events go to `worktree.repo.<id>.changed` and
/// `worktree.repo.<id>.pull`.
pub const BUS_SUBJECT_PREFIX: &str = "worktree";
/// The health check fails when the NATS connection is down for longer.
pub const BUS_HEALTH_GRACE: Duration = Duration::from_secs(5);

/// Web sessions end after this time.
pub const SESSION_DAYS: u64 = 14;
