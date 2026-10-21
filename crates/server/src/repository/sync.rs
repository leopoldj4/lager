//! Bringing a cache to a storage generation, and rebuilding it.
//!
//! Accepted changes are found only through the head's index, never by
//! listing storage.

use std::path::Path;
use std::sync::Arc;

use lager_core::history::{Chain, chain_state, load_chain, load_entries};
use serde::{Deserialize, Serialize};

use super::{CacheInstance, CacheState, Controller, Known, RepoError};
use crate::metrics::StepTimer;
use lager_core::model::{
    EntryId, Head, IncarnationId, LogEntry, PackRef, RefState, RefUpdate, RepoId,
};
use lager_core::storage::{ETag, Storage};

/// Saved in each cache after a complete local change. Never trusted alone.
#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    repository: RepoId,
    incarnation: IncarnationId,
    generation: u64,
    entry: EntryId,
}

const MARKER: &str = "lager-state.json";

/// Downloads a pack, checks it, and adds it to the repository.
pub async fn install_pack(
    storage: &Storage,
    git: &crate::git::Git,
    tmp: &Path,
    repo: RepoId,
    dir: &Path,
    pack: &PackRef,
) -> Result<(), RepoError> {
    let file = tmp.join(format!("dl-{}.pack", uuid::Uuid::new_v4().simple()));
    let result = async {
        storage.fetch_pack(repo, pack, &file).await?;
        git.index_pack(dir, &file).await?;
        Ok::<_, RepoError>(())
    }
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    result
}

impl Controller {
    /// Applies entries after the cache's generation (read procedure step 3).
    pub(crate) async fn apply_new_entries(
        &self,
        st: &mut CacheState,
        head: &Head,
        etag: &ETag,
    ) -> Result<(), RepoError> {
        let _t = StepTimer::new(&self.svc.metrics, "cache_update");
        let known = st.known.clone().expect("checked by caller");
        let cache = st.cache.clone().expect("checked by caller");
        if !head.contains(known.head.generation, known.entry) {
            return Err(RepoError::Internal(
                "cache generation is not in the storage index".into(),
            ));
        }
        let entries = load_entries(&self.svc.storage, head, known.head.generation + 1).await?;
        let _space = self
            .svc
            .reserve_for_packs(entries.iter().flat_map(|e| e.packs.iter()))?;
        let mut refs = known.refs.clone();
        for e in &entries {
            self.apply_entry_locally(&cache, &mut refs, e).await?;
            write_marker(
                &cache.dir,
                head.repository,
                head.incarnation,
                e.generation,
                e.id,
            )
            .await?;
        }
        self.svc
            .metrics
            .add("entries_applied", entries.len() as u64);
        self.set_known(
            st,
            Known {
                head: head.clone(),
                entry: head.latest_entry(),
                etag: Some(etag.clone()),
                refs,
            },
        );
        Ok(())
    }

    async fn apply_entry_locally(
        &self,
        cache: &CacheInstance,
        refs: &mut RefState,
        e: &LogEntry,
    ) -> Result<(), RepoError> {
        for p in &e.packs {
            install_pack(
                &self.svc.storage,
                &self.svc.git,
                &self.svc.tmp_dir(),
                self.repo,
                &cache.dir,
                p,
            )
            .await?;
        }
        let before = refs.head.clone();
        refs.apply_entry(e).map_err(RepoError::Internal)?;
        if !e.refs.is_empty() {
            let txn = self
                .svc
                .git
                .prepare_ref_transaction(&cache.dir, cache.format, &e.refs)
                .await?;
            txn.commit().await?;
        }
        if refs.head != before
            && let Some(h) = &refs.head
        {
            self.svc.git.set_head(&cache.dir, h).await?;
        }
        Ok(())
    }

    /// Builds a new cache in a temporary directory, checks it, and replaces
    /// the current cache atomically. Never serves a partial copy.
    pub(crate) async fn rebuild(
        &self,
        st: &mut CacheState,
        head: &Head,
        etag: &ETag,
    ) -> Result<(), RepoError> {
        let _t = StepTimer::new(&self.svc.metrics, "cache_rebuild");
        self.svc.metrics.inc("cache_rebuilds");
        tracing::info!(repo = %self.repo, generation = head.generation, "rebuilding cache from storage");
        let chain = load_chain(&self.svc.storage, head).await?;
        let refs = chain_state(&chain).map_err(RepoError::Internal)?;
        let _space = self.svc.reserve_for_packs(
            chain
                .snapshot
                .iter()
                .flat_map(|s| s.packs.iter())
                .chain(chain.entries.iter().flat_map(|e| e.packs.iter())),
        )?;
        let base = self.svc.repo_cache_dir(self.repo);
        tokio::fs::create_dir_all(&base).await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let tmp = base.join(format!("{id}.tmp"));
        let result = self.build_into(&tmp, head, &chain, &refs).await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Err(e);
        }
        let dir = base.join(format!("{id}.git"));
        tokio::fs::rename(&tmp, &dir).await?;
        st.poison();
        st.cache = Some(Arc::new(CacheInstance::new(dir, head.object_format)));
        self.set_known(
            st,
            Known {
                head: head.clone(),
                entry: head.latest_entry(),
                etag: Some(etag.clone()),
                refs,
            },
        );
        Ok(())
    }

    async fn build_into(
        &self,
        dir: &Path,
        head: &Head,
        chain: &Chain,
        refs: &RefState,
    ) -> Result<(), RepoError> {
        let git = &self.svc.git;
        git.init_cache_repo(dir, head.object_format).await?;
        let packs = chain
            .snapshot
            .iter()
            .flat_map(|s| s.packs.iter())
            .chain(chain.entries.iter().flat_map(|e| e.packs.iter()));
        for p in packs {
            install_pack(
                &self.svc.storage,
                git,
                &self.svc.tmp_dir(),
                self.repo,
                dir,
                p,
            )
            .await?;
        }
        let creates: Vec<RefUpdate> = refs
            .refs
            .iter()
            .map(|(n, o)| RefUpdate {
                name: n.clone(),
                old: None,
                new: Some(o.clone()),
            })
            .collect();
        if !creates.is_empty() {
            git.prepare_ref_transaction(dir, head.object_format, &creates)
                .await?
                .commit()
                .await?;
        }
        if let Some(h) = &refs.head {
            git.set_head(dir, h).await?;
        }
        git.check_connectivity(dir).await?;
        write_marker(
            dir,
            head.repository,
            head.incarnation,
            head.generation,
            head.latest_entry(),
        )
        .await?;
        Ok(())
    }

    /// Adopts a cache left by an earlier process only after a full check:
    /// its refs and Git `HEAD` must equal the stored state at its generation,
    /// and all reachable objects must exist.
    pub(crate) async fn adopt_leftover(&self, st: &mut CacheState, head: &Head) {
        let base = self.svc.repo_cache_dir(self.repo);
        let Ok(mut rd) = tokio::fs::read_dir(&base).await else {
            return;
        };
        let mut dirs = Vec::new();
        while let Ok(Some(e)) = rd.next_entry().await {
            dirs.push(e.path());
        }
        for dir in dirs {
            if dir.extension().and_then(|e| e.to_str()) == Some("git")
                && st.cache.is_none()
                && let Ok(known) = self.check_leftover(&dir, head).await
            {
                tracing::info!(repo = %self.repo, generation = known.head.generation, "adopted checked cache");
                st.cache = Some(Arc::new(CacheInstance::new(
                    dir.clone(),
                    head.object_format,
                )));
                self.set_known(st, known);
                continue;
            }
            // Unchecked or damaged caches are removed.
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }

    async fn check_leftover(&self, dir: &Path, head: &Head) -> Result<Known, RepoError> {
        // Lock files show a ref change that did not finish: a stop during a
        // local transaction. Such a cache is never used.
        if let Some(lock) = find_lock_file(dir).await? {
            return Err(RepoError::Internal(format!(
                "incomplete local change: {}",
                lock.display()
            )));
        }
        let data = tokio::fs::read(dir.join(MARKER)).await?;
        let m: Marker =
            serde_json::from_slice(&data).map_err(|e| RepoError::Internal(e.to_string()))?;
        if m.repository != head.repository
            || m.incarnation != head.incarnation
            || m.generation > head.generation
        {
            return Err(RepoError::Internal("marker does not match".into()));
        }
        // Reconstruct the expected state at the marker's generation.
        let chain = load_chain(&self.svc.storage, head).await?;
        let mut state = RefState::default();
        if let Some(s) = &chain.snapshot {
            if s.base_generation > m.generation {
                return Err(RepoError::Internal("cache older than the snapshot".into()));
            }
            state.refs = s.refs.clone();
            state.head = Some(s.head.clone());
        }
        let mut at: Option<&LogEntry> = None;
        for e in chain
            .entries
            .iter()
            .take_while(|e| e.generation <= m.generation)
        {
            state.apply_entry(e).map_err(RepoError::Internal)?;
            at = Some(e);
        }
        let at_id = at
            .map(|e| e.id)
            .or(chain.snapshot.as_ref().map(|s| s.base_entry));
        if at_id != Some(m.entry) {
            return Err(RepoError::Internal(
                "marker entry is not in the history".into(),
            ));
        }
        let local = self.svc.git.refs(dir, head.object_format).await?;
        let local_head = self.svc.git.head_target(dir).await?;
        if local != state.refs
            || Some(local_head.as_str()) != state.head.as_ref().map(|h| h.as_str())
        {
            return Err(RepoError::Internal("local refs differ from storage".into()));
        }
        self.svc.git.check_connectivity(dir).await?;
        let mut marker_head = head.clone();
        marker_head.generation = m.generation;
        if m.generation != head.generation {
            // Unknown revision of that older head: the next check differs and
            // updates the cache from the marker's generation.
            marker_head.revision = lager_core::model::RevisionId::new();
            marker_head.snapshot = None;
        }
        Ok(Known {
            head: marker_head,
            entry: m.entry,
            etag: None,
            refs: state,
        })
    }
}

/// A Git lock file for refs or `HEAD`, if one exists.
async fn find_lock_file(dir: &Path) -> std::io::Result<Option<std::path::PathBuf>> {
    for name in ["HEAD.lock", "packed-refs.lock"] {
        if tokio::fs::try_exists(dir.join(name)).await? {
            return Ok(Some(dir.join(name)));
        }
    }
    let mut stack = vec![dir.join("refs")];
    while let Some(d) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&d).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        while let Some(e) = rd.next_entry().await? {
            let p = e.path();
            if e.file_type().await?.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "lock") {
                return Ok(Some(p));
            }
        }
    }
    Ok(None)
}

pub(crate) async fn write_marker(
    dir: &Path,
    repository: RepoId,
    incarnation: IncarnationId,
    generation: u64,
    entry: EntryId,
) -> std::io::Result<()> {
    let m = Marker {
        repository,
        incarnation,
        generation,
        entry,
    };
    let tmp = dir.join(format!("{MARKER}.tmp"));
    tokio::fs::write(&tmp, serde_json::to_vec(&m).expect("serializes")).await?;
    tokio::fs::rename(&tmp, dir.join(MARKER)).await
}
