//! Controllers, caches, recovery, request results, and locks.
//!
//! Each repository has one [`Controller`] per server. The controller
//! - permits one local write operation at a time (`write_op`),
//! - brings the cache to the storage head under the state lock,
//! - publishes complete read views per generation, so readers never observe
//!   part of a ref transaction and long reads do not block writes.

pub mod push;
pub mod sync;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;

use crate::git::Git;
use crate::metrics::Metrics;
use worktree_core::model::{
    Head, ObjectFormat, Oid, Operation, RefName, RefState, RepoId, RepoState, RequestId,
};
use worktree_core::repo::{Access, RefRule};
use worktree_core::storage::{ETag, HeadRead, Storage, StorageError};

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("repository not found")]
    NotFound,
    #[error("repository deleted")]
    Deleted,
    #[error("storage unavailable: {0}")]
    Unavailable(String),
    /// Not enough local disk space for the work.
    #[error("{0}")]
    NoSpace(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<StorageError> for RepoError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::Unavailable(m) => RepoError::Unavailable(m),
            other => RepoError::Internal(other.to_string()),
        }
    }
}

impl From<crate::git::GitError> for RepoError {
    fn from(e: crate::git::GitError) -> Self {
        RepoError::Internal(e.to_string())
    }
}

impl From<std::io::Error> for RepoError {
    fn from(e: std::io::Error) -> Self {
        RepoError::Internal(e.to_string())
    }
}

/// Shared parts of one server.
pub struct Services {
    pub storage: Storage,
    pub git: Git,
    pub cache_root: PathBuf,
    pub metrics: Arc<Metrics>,
    /// Change events of this and the other servers.
    pub bus: Arc<crate::bus::Bus>,
    /// Disk space for receipts and cache reconstruction.
    pub disk: Arc<crate::disk::DiskReservations>,
}

impl Services {
    pub fn tmp_dir(&self) -> PathBuf {
        self.cache_root.join("tmp")
    }

    fn repo_cache_dir(&self, repo: RepoId) -> PathBuf {
        self.cache_root.join("repos").join(repo.to_string())
    }

    fn view_dir(&self, repo: RepoId) -> PathBuf {
        self.cache_root.join("views").join(repo.to_string())
    }

    /// Reserves disk space for downloading and indexing packs.
    pub(crate) fn reserve_for_packs<'a>(
        &self,
        packs: impl Iterator<Item = &'a worktree_core::model::PackRef>,
    ) -> Result<crate::disk::DiskReservation, RepoError> {
        let bytes: u64 = packs.map(|p| p.size).sum();
        self.disk.reserve(bytes.saturating_mul(2)).map_err(|m| {
            self.metrics.inc("disk_limit_rejections");
            RepoError::NoSpace(m)
        })
    }
}

/// Permission of the user of a push, read again before each attempt.
#[derive(Clone, Debug)]
pub struct PushAccess {
    /// The repository that the name maps to now.
    pub repo: RepoId,
    pub level: Access,
    pub ref_rules: Vec<RefRule>,
}

impl PushAccess {
    pub fn none() -> Self {
        Self {
            repo: RepoId(uuid::Uuid::nil()),
            level: Access::None,
            ref_rules: vec![],
        }
    }
}

/// Reads the current permission rules. `Err` when they cannot be read.
pub type AccessCheck =
    Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<PushAccess, String>> + Send + Sync>;

/// Told the new default branch when a first push chose it, to update copies
/// of the setting such as the catalog record.
pub type DefaultBranchChanged =
    Arc<dyn Fn(RepoId, RefName) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// Who asks for a ref change.
#[derive(Clone, Debug)]
pub enum Origin {
    /// A Git client through `receive-pack`. Clients cannot change the refs
    /// of pull requests.
    Client,
    /// The server itself, for pull requests: the operation of the entry.
    Server(Operation),
}

/// A push operation that waits for the hook. Found by its access token.
/// The server's own ref changes use it too, without the hook.
#[derive(Clone)]
pub struct PendingPush {
    pub repo: RepoId,
    pub user: String,
    /// Also the request UUID of the log entry.
    pub request: RequestId,
    pub access: AccessCheck,
    /// The cache the objects were received into.
    pub cache: Arc<CacheInstance>,
    pub default_branch_changed: Option<DefaultBranchChanged>,
    pub origin: Origin,
    /// Read dependencies checked against each CAS attempt without changing refs.
    pub ref_guards: Vec<(RefName, Option<Oid>)>,
}

pub struct Registry {
    pub svc: Arc<Services>,
    controllers: StdMutex<HashMap<RepoId, Arc<Controller>>>,
    ops: StdMutex<HashMap<String, PendingPush>>,
}

impl Registry {
    /// Prepares the cache directory. Caches from an earlier process are not
    /// trusted: each is fully checked against storage before use.
    pub fn new(svc: Arc<Services>) -> std::io::Result<Arc<Registry>> {
        std::fs::create_dir_all(svc.cache_root.join("repos"))?;
        // Views and temporary files never survive a restart.
        let _ = std::fs::remove_dir_all(svc.cache_root.join("views"));
        let _ = std::fs::remove_dir_all(svc.tmp_dir());
        std::fs::create_dir_all(svc.cache_root.join("views"))?;
        std::fs::create_dir_all(svc.tmp_dir())?;
        Ok(Arc::new(Registry {
            svc,
            controllers: StdMutex::new(HashMap::new()),
            ops: StdMutex::new(HashMap::new()),
        }))
    }

    pub fn controller(&self, repo: RepoId) -> Arc<Controller> {
        let mut map = self.controllers.lock().unwrap();
        map.entry(repo)
            .or_insert_with(|| Arc::new(Controller::new(repo, self.svc.clone())))
            .clone()
    }

    /// Registers a push and returns its access token.
    pub fn register_push(self: &Arc<Self>, op: PendingPush) -> OpToken {
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        self.ops.lock().unwrap().insert(token.clone(), op);
        OpToken {
            registry: self.clone(),
            token,
        }
    }

    /// Takes the operation for a token. Each token works once.
    pub fn take_push(&self, token: &str) -> Option<PendingPush> {
        self.ops.lock().unwrap().remove(token)
    }

    /// The controllers of repositories that this server has used.
    pub fn controllers(&self) -> Vec<Arc<Controller>> {
        self.controllers.lock().unwrap().values().cloned().collect()
    }

    /// Removes caches that no active operation uses and that were idle.
    pub async fn evict_idle(&self, idle: std::time::Duration) -> usize {
        let candidates: Vec<Arc<Controller>> =
            self.controllers.lock().unwrap().values().cloned().collect();
        let mut n = 0;
        for c in candidates {
            if c.idle_for() >= idle && c.try_evict().await {
                n += 1;
            }
        }
        n
    }
}

/// Removes the token when the request ends.
pub struct OpToken {
    registry: Arc<Registry>,
    pub token: String,
}

impl Drop for OpToken {
    fn drop(&mut self) {
        self.registry.ops.lock().unwrap().remove(&self.token);
    }
}

/// One local repository copy. The directory is removed when the instance is
/// replaced and the last user releases it.
#[derive(Debug)]
pub struct CacheInstance {
    pub dir: PathBuf,
    pub format: ObjectFormat,
    obsolete: AtomicBool,
}

impl CacheInstance {
    fn new(dir: PathBuf, format: ObjectFormat) -> Self {
        Self {
            dir,
            format,
            obsolete: AtomicBool::new(false),
        }
    }

    fn retire(&self) {
        self.obsolete.store(true, Ordering::SeqCst);
    }
}

impl Drop for CacheInstance {
    fn drop(&mut self) {
        if self.obsolete.load(Ordering::SeqCst) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// A complete, fixed ref view of one generation. It shares the objects of
/// its cache instance through `objects/info/alternates`.
#[derive(Debug)]
pub struct ReadView {
    pub dir: PathBuf,
    pub generation: u64,
    _cache: Arc<CacheInstance>,
}

impl ReadView {
    pub fn format(&self) -> ObjectFormat {
        self._cache.format
    }
}

impl Drop for ReadView {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What the cache holds.
#[derive(Clone, Debug)]
pub struct Known {
    /// The head of the cache's generation. Its index can be newer than that
    /// generation for a cache from an earlier process; use `entry`.
    pub head: Head,
    /// The entry of the cache's generation.
    pub entry: worktree_core::model::EntryId,
    /// `None` when the accepted head's ETag is not known.
    pub etag: Option<ETag>,
    pub refs: RefState,
}

#[derive(Default)]
pub struct CacheState {
    pub cache: Option<Arc<CacheInstance>>,
    pub known: Option<Known>,
    view: Option<Arc<ReadView>>,
    /// An earlier process may have left a cache on disk.
    leftover_checked: bool,
}

impl CacheState {
    /// Blocks access to the cache until it is rebuilt from storage.
    pub fn poison(&mut self) {
        if let Some(c) = self.cache.take() {
            c.retire();
        }
        self.known = None;
        self.view = None;
    }
}

pub struct Controller {
    pub repo: RepoId,
    pub(crate) svc: Arc<Services>,
    pub(crate) write_op: Mutex<()>,
    pub(crate) state: Mutex<CacheState>,
    last_etag: StdMutex<Option<ETag>>,
    last_used_ms: AtomicU64,
    /// This server accepted a push since the last snapshot check, so it
    /// creates the next automatic snapshot.
    pub(crate) pushed_here: AtomicBool,
}

impl Controller {
    fn new(repo: RepoId, svc: Arc<Services>) -> Self {
        Self {
            repo,
            svc,
            write_op: Mutex::new(()),
            state: Mutex::new(CacheState::default()),
            last_etag: StdMutex::new(None),
            last_used_ms: AtomicU64::new(worktree_core::model::now_ms()),
            pushed_here: AtomicBool::new(false),
        }
    }

    fn touch(&self) {
        self.last_used_ms
            .store(worktree_core::model::now_ms(), Ordering::Relaxed);
    }

    fn idle_for(&self) -> std::time::Duration {
        let last = self.last_used_ms.load(Ordering::Relaxed);
        std::time::Duration::from_millis(worktree_core::model::now_ms().saturating_sub(last))
    }

    async fn try_evict(&self) -> bool {
        let Ok(_w) = self.write_op.try_lock() else {
            return false;
        };
        let Ok(mut st) = self.state.try_lock() else {
            return false;
        };
        let in_use = st
            .cache
            .as_ref()
            .map(|c| Arc::strong_count(c) > 1 + st.view.is_some() as usize)
            .unwrap_or(false);
        let view_in_use = st
            .view
            .as_ref()
            .map(|v| Arc::strong_count(v) > 1)
            .unwrap_or(false);
        if in_use || view_in_use || st.cache.is_none() {
            return false;
        }
        st.poison();
        *self.last_etag.lock().unwrap() = None;
        true
    }

    pub(crate) fn set_known(&self, st: &mut CacheState, known: Known) {
        *self.last_etag.lock().unwrap() = known.etag.clone();
        if st.view.as_ref().map(|v| v.generation) != Some(known.head.generation) {
            st.view = None;
        }
        st.known = Some(known);
    }

    /// Reads the storage head and brings the cache to it (read procedure,
    /// steps 2-4). The returned state is at least as new as the head.
    pub async fn sync_to_storage(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, CacheState>, RepoError> {
        self.touch();
        let known_etag = self.last_etag.lock().unwrap().clone();
        let read = self
            .svc
            .storage
            .read_head(self.repo, known_etag.as_ref())
            .await?;
        let mut st = self.state.lock().await;
        let (head, etag) = match read {
            HeadRead::Missing => {
                st.poison();
                return Err(RepoError::NotFound);
            }
            HeadRead::Unchanged => {
                let current = st.known.as_ref().and_then(|k| k.etag.clone());
                if st.cache.is_some() && current.is_some() && current == known_etag {
                    return Ok(st);
                }
                drop(st);
                // The local state changed while the request was in flight.
                let Some((head, etag)) = self.svc.storage.load_head(self.repo).await? else {
                    return Err(RepoError::NotFound);
                };
                st = self.state.lock().await;
                (head, etag)
            }
            HeadRead::Found { head, etag } => (head, etag),
        };
        self.ensure_current(&mut st, &head, &etag).await?;
        Ok(st)
    }

    /// The generation that the cache holds, if it holds one.
    pub async fn cached_generation(&self) -> Option<u64> {
        let st = self.state.lock().await;
        st.cache.as_ref()?;
        st.known.as_ref().map(|k| k.head.generation)
    }

    /// The accepted head that the cache holds.
    pub async fn cached_head(&self) -> Option<Head> {
        let st = self.state.lock().await;
        st.cache.as_ref()?;
        st.known.as_ref().map(|k| k.head.clone())
    }

    /// Returns a read view that includes every push accepted before the call.
    pub async fn prepare_read(&self) -> Result<Arc<ReadView>, RepoError> {
        let mut st = self.sync_to_storage().await?;
        self.view(&mut st).await
    }

    /// Syncs the cache for receiving objects and returns the instance.
    pub async fn prepare_write(&self) -> Result<(Arc<CacheInstance>, Arc<ReadView>), RepoError> {
        let mut st = self.sync_to_storage().await?;
        let view = self.view(&mut st).await?;
        let cache = st
            .cache
            .clone()
            .ok_or_else(|| RepoError::Internal("no cache".into()))?;
        Ok((cache, view))
    }

    /// For a ref change by the server: syncs the cache and returns it, a
    /// read view, and the refs of the view's generation.
    pub async fn prepare_change(
        &self,
    ) -> Result<(Arc<CacheInstance>, Arc<ReadView>, RefState), RepoError> {
        let mut st = self.sync_to_storage().await?;
        let view = self.view(&mut st).await?;
        let cache = st
            .cache
            .clone()
            .ok_or_else(|| RepoError::Internal("no cache".into()))?;
        let refs = st
            .known
            .as_ref()
            .ok_or_else(|| RepoError::Internal("cache not ready".into()))?
            .refs
            .clone();
        Ok((cache, view, refs))
    }

    async fn view(&self, st: &mut CacheState) -> Result<Arc<ReadView>, RepoError> {
        let known = st
            .known
            .clone()
            .ok_or_else(|| RepoError::Internal("cache not ready".into()))?;
        if let Some(v) = &st.view
            && v.generation == known.head.generation
        {
            return Ok(v.clone());
        }
        let cache = st
            .cache
            .clone()
            .ok_or_else(|| RepoError::Internal("cache not ready".into()))?;
        let dir = self.svc.view_dir(self.repo).join(format!(
            "{}-{}.git",
            known.head.generation,
            uuid::Uuid::new_v4().simple()
        ));
        write_view(&dir, &cache, &known.refs).await?;
        let view = Arc::new(ReadView {
            dir,
            generation: known.head.generation,
            _cache: cache,
        });
        st.view = Some(view.clone());
        Ok(view)
    }

    /// Brings the cache to `head`, or rebuilds it.
    pub(crate) async fn ensure_current(
        &self,
        st: &mut CacheState,
        head: &Head,
        etag: &ETag,
    ) -> Result<(), RepoError> {
        if head.state == RepoState::Deleted {
            st.poison();
            return Err(RepoError::Deleted);
        }
        if !st.leftover_checked {
            st.leftover_checked = true;
            if st.cache.is_none() {
                self.adopt_leftover(st, head).await;
            }
        }
        if let (Some(_), Some(k)) = (&st.cache, &st.known)
            && k.head.incarnation == head.incarnation
        {
            if k.head.generation == head.generation {
                if k.head.revision == head.revision {
                    let mut k = k.clone();
                    k.etag = Some(etag.clone());
                    self.set_known(st, k);
                    return Ok(());
                }
            } else if k.head.generation > head.generation {
                // A newer local state from a concurrent operation.
                return Ok(());
            } else if !head.contains(k.head.generation, k.entry) {
                // A snapshot folded the cache's generation out of the index.
                tracing::info!(repo = %self.repo, generation = k.head.generation, "cache is behind the snapshot; rebuilding");
                st.poison();
            } else {
                match self.apply_new_entries(st, head, etag).await {
                    Ok(()) => return Ok(()),
                    Err(RepoError::Unavailable(m)) => return Err(RepoError::Unavailable(m)),
                    Err(e) => {
                        tracing::warn!(repo = %self.repo, error = %e, "cache update failed; rebuilding");
                        self.svc.metrics.inc("cache_update_failures");
                        st.poison();
                    }
                }
            }
        }
        self.rebuild(st, head, etag).await
    }
}

async fn write_view(dir: &Path, cache: &CacheInstance, refs: &RefState) -> std::io::Result<()> {
    use tokio::fs;
    fs::create_dir_all(dir.join("objects/info")).await?;
    fs::create_dir_all(dir.join("objects/pack")).await?;
    fs::create_dir_all(dir.join("refs/heads")).await?;
    fs::create_dir_all(dir.join("refs/tags")).await?;
    let head = refs
        .head
        .as_ref()
        .map(|h| h.as_str())
        .unwrap_or("refs/heads/main");
    fs::write(dir.join("HEAD"), format!("ref: {head}\n")).await?;
    let mut config = String::from("[core]\n\trepositoryformatversion = 0\n\tbare = true\n");
    config.push_str(crate::git::read_config());
    fs::write(dir.join("config"), config).await?;
    fs::write(
        dir.join("objects/info/alternates"),
        format!("{}\n", cache.dir.join("objects").display()),
    )
    .await?;
    // Not "peeled": Git peels annotated tags itself.
    let mut packed = String::from("# pack-refs with: sorted \n");
    for (name, oid) in &refs.refs {
        packed.push_str(&format!("{oid} {name}\n"));
    }
    fs::write(dir.join("packed-refs"), packed).await?;
    Ok(())
}
