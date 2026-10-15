//! The push procedure.
//!
//! The head CAS makes a push visible. Before it, any failure cancels the
//! push. After it, the push is retained, even if local completion fails.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::{CacheInstance, Controller, Known, Origin, PendingPush, PushAccess, RepoError};
use crate::git::hook::{HookCommand, HookResponse, HookResult};
use crate::metrics::StepTimer;
use worktree_core::cas::{Resolution, jitter, resolve_unknown_cas};
use worktree_core::limits;
use worktree_core::model::{
    EntryId, FORMAT_VERSION, Head, LOG_FORMAT_VERSION, LogEntry, ObjectFormat, Oid, Operation,
    PackRef, Receipt, RefName, RefState, RefUpdate, RepoState, RequestId, now_ms, request_checksum,
};
use worktree_core::repo::{Access, ref_rule};
use worktree_core::storage::{CasOutcome, Storage};

/// One push request from the hook.
struct Request {
    id: RequestId,
    checksum: String,
    updates: Vec<RefUpdate>,
    /// No CAS is sent after this time: uploads of an expired operation may
    /// already be removed (see `worktree_core::cleanup::cleanup`).
    deadline: Instant,
}

/// The pack of the current attempt: the parent entry it was built against,
/// the pack (none if no objects are new), and its upload time.
type AttemptPack = Option<(EntryId, Option<PackRef>, u64)>;

enum Attempt {
    Done(HookResponse),
    Retry(&'static str),
}

/// Whether a change may reference packs uploaded at a given time.
pub(crate) enum Fence {
    Clear,
    /// A pack is on the removal list of an expired fence: upload it again.
    Reupload,
    /// A pack is on the removal list of an active fence.
    Wait,
}

/// Checks packs against the head's latest removal fence. Earlier fences
/// expired before the latest one was attached, and every pack of this change
/// was uploaded after the head was read.
pub(crate) async fn fence_check(
    storage: &Storage,
    head: &Head,
    packs: &[PackRef],
    uploaded_at_ms: u64,
) -> Result<Fence, RepoError> {
    let Some(r) = &head.removal else {
        return Ok(Fence::Clear);
    };
    let safe_after = r.reuse_after_ms;
    if packs.is_empty() || uploaded_at_ms > safe_after {
        return Ok(Fence::Clear);
    }
    let fence = storage.get_entry(head.repository, r.entry).await?;
    let Operation::Removal { packs: removed, .. } = &fence.operation else {
        return Err(RepoError::Internal(
            "removal pointer names another entry".into(),
        ));
    };
    if !packs.iter().any(|p| removed.contains(&p.sha256)) {
        return Ok(Fence::Clear);
    }
    Ok(if now_ms() > safe_after {
        Fence::Reupload
    } else {
        Fence::Wait
    })
}

impl Controller {
    /// Runs the push procedure for commands that the hook received.
    pub async fn push(&self, op: &PendingPush, commands: Vec<HookCommand>) -> HookResponse {
        let _t = StepTimer::new(&self.svc.metrics, "push_total");
        self.svc.metrics.inc("pushes");
        let names: Vec<String> = commands.iter().map(|c| c.name.clone()).collect();
        let format = op.cache.format;
        let updates = match parse_commands(format, &commands) {
            Ok(u) => u,
            Err(m) => return reject_all(&names, &m),
        };
        if matches!(op.origin, Origin::Client) {
            let managed: Vec<(String, String)> = updates
                .iter()
                .filter(|u| u.name.is_pull())
                .map(|u| {
                    (
                        u.name.to_string(),
                        "refs/pull/ and refs/server/ are kept by the server".into(),
                    )
                })
                .collect();
            if !managed.is_empty() {
                return reject_some(&names, &managed);
            }
        }
        let _w = self.write_op.lock().await;
        let req = Request {
            id: op.request,
            checksum: request_checksum(&op.user, &updates),
            updates,
            deadline: Instant::now() + limits::PUSH_DEADLINE,
        };
        let mut pack: AttemptPack = None;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.push_attempt(op, &req, &names, &mut pack).await {
                Ok(Attempt::Done(r)) => return r,
                Ok(Attempt::Retry(why)) => {
                    self.svc.metrics.inc("push_retries");
                    tracing::debug!(repo = %self.repo, request = %req.id, attempt, why, "push retry");
                }
                Err(RepoError::Unavailable(m)) => {
                    self.svc.metrics.inc("push_storage_unavailable");
                    return reject_all(&names, &format!("storage unavailable: {m}"));
                }
                Err(RepoError::NotFound) => {
                    return reject_all(&names, "repository not found");
                }
                Err(RepoError::Deleted) => return reject_all(&names, "repository deleted"),
                Err(RepoError::NoSpace(m)) => return reject_all(&names, &m),
                Err(RepoError::Internal(m)) => {
                    tracing::error!(repo = %self.repo, request = %req.id, error = %m, "push failed");
                    return reject_all(&names, &format!("internal error (request {})", req.id));
                }
            }
            if attempt >= limits::PUSH_RETRY_LIMIT || Instant::now() >= req.deadline {
                self.svc.metrics.inc("push_conflicts_exhausted");
                return reject_all(&names, "concurrent changes; retry the push");
            }
            tokio::time::sleep(jitter(attempt)).await;
        }
    }

    async fn push_attempt(
        &self,
        op: &PendingPush,
        req: &Request,
        names: &[String],
        pack: &mut AttemptPack,
    ) -> Result<Attempt, RepoError> {
        let svc = &self.svc;
        // Permission rules can change between attempts.
        let access = (op.access)().await.map_err(RepoError::Unavailable)?;
        if access.repo != self.repo {
            return Err(RepoError::NotFound);
        }
        if access.level < Access::Write {
            return Ok(Attempt::Done(reject_all(names, "permission denied")));
        }

        // Steps 5-6: read the head, bring the local copy to it.
        let (head, etag) = {
            let _t = StepTimer::new(&svc.metrics, "push_read_head");
            svc.storage
                .load_head(self.repo)
                .await?
                .ok_or(RepoError::NotFound)?
        };
        if head.state == RepoState::Deleted {
            return Err(RepoError::Deleted);
        }
        let base: Known = {
            let _t = StepTimer::new(&svc.metrics, "push_sync");
            let mut st = self.state.lock().await;
            self.ensure_current(&mut st, &head, &etag).await?;
            if !same_cache(&st.cache, &op.cache) {
                return Ok(Attempt::Done(reject_all(
                    names,
                    "server cache was replaced; retry the push",
                )));
            }
            let k = st.known.clone().expect("current");
            if k.head.generation != head.generation {
                return Ok(Attempt::Retry("local state newer than head read"));
            }
            k
        };
        let dir = &op.cache.dir;

        // Read dependencies and updates must be checked against the same head.
        if op
            .ref_guards
            .iter()
            .any(|(name, oid)| base.refs.refs.get(name) != oid.as_ref())
        {
            return Ok(Attempt::Done(reject_all(
                names,
                "reviewed source branch changed",
            )));
        }

        // Step 8: check ref names, requested changes, and objects.
        let next = match base.refs.check_updates(&req.updates) {
            Ok(n) => n,
            Err(rejections) => {
                let msgs: Vec<(String, String)> =
                    rejections.into_iter().map(|r| (r.name, r.reason)).collect();
                return Ok(Attempt::Done(reject_some(names, &msgs)));
            }
        };
        if let Some(r) = self
            .check_objects_and_rules(dir, &base.refs, &req.updates, &access)
            .await?
        {
            return Ok(Attempt::Done(reject_some(names, &r)));
        }

        // Steps 10-12: a pack relative to the parent state, uploaded and checked.
        if pack.as_ref().map(|(e, _, _)| *e) != Some(base.entry) {
            let p = self
                .build_and_store_pack(dir, &base.refs, &req.updates)
                .await?;
            *pack = Some((base.entry, p, now_ms()));
        }
        let (packs, uploaded_at): (Vec<PackRef>, u64) = match pack.as_ref() {
            Some((_, p, at)) => (p.clone().into_iter().collect(), *at),
            None => (vec![], now_ms()),
        };
        match fence_check(&svc.storage, &head, &packs, uploaded_at).await? {
            Fence::Clear => {}
            Fence::Reupload => {
                *pack = None;
                return Ok(Attempt::Retry("pack was on a removal list"));
            }
            Fence::Wait => {
                *pack = None;
                svc.metrics.inc("push_removal_fence_waits");
                return Ok(Attempt::Done(reject_all(
                    names,
                    "stored data removal in progress; retry the push later",
                )));
            }
        }

        // Step 13: an entry for the generation after the observed head. Only
        // the head's index makes it part of the history.
        let entry = LogEntry {
            format: LOG_FORMAT_VERSION,
            repository: self.repo,
            incarnation: head.incarnation,
            id: EntryId::new(),
            generation: base.head.generation + 1,
            request: req.id,
            request_checksum: req.checksum.clone(),
            operation: match &op.origin {
                Origin::Client => Operation::Push,
                Origin::Server(o) => o.clone(),
            },
            refs: req.updates.clone(),
            packs,
            user: op.user.clone(),
            // Permissions are not versioned.
            rules_version: 0,
            received_at_ms: now_ms(),
        };
        {
            let _t = StepTimer::new(&svc.metrics, "push_put_entry");
            svc.storage.put_entry(&entry).await?;
        }

        // A first push that does not contain the default branch chooses it,
        // so that clones check out what was pushed.
        let adopt = adopted_default_branch(&base.refs, &next);

        // Steps 14-18 under the state lock.
        let mut st = self.state.lock().await;
        if st.known.as_ref().map(|k| k.head.revision) != Some(base.head.revision)
            || !same_cache(&st.cache, &op.cache)
        {
            return Ok(Attempt::Retry("local state changed"));
        }
        let txn = match svc
            .git
            .prepare_ref_transaction(dir, op.cache.format, &req.updates)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                // Local refs do not match the stored state they claim to hold.
                tracing::warn!(repo = %self.repo, error = %e, "ref transaction prepare failed; rebuilding cache");
                st.poison();
                return Ok(Attempt::Done(reject_all(
                    names,
                    "server cache was replaced; retry the push",
                )));
            }
        };
        if Instant::now() >= req.deadline {
            let _ = txn.abort().await;
            svc.metrics.inc("push_deadline_expired");
            return Ok(Attempt::Done(reject_all(
                names,
                "push took too long; retry the push",
            )));
        }
        let new_head = head.advance(&entry);
        let expected = base.etag.clone().unwrap_or(etag);
        let outcome = {
            let _t = StepTimer::new(&svc.metrics, "push_cas");
            svc.storage.replace_head(&new_head, &expected).await
        };
        let accepted_etag = match outcome {
            CasOutcome::Accepted(e) => Some(e),
            CasOutcome::Conflict => {
                svc.metrics.inc("cas_conflicts");
                let _ = txn.abort().await;
                return Ok(Attempt::Retry("cas conflict"));
            }
            CasOutcome::Unknown(m) => {
                svc.metrics.inc("cas_unknown");
                tracing::warn!(repo = %self.repo, request = %req.id, error = %m, "CAS result unknown; resolving");
                let until = Instant::now() + limits::UNKNOWN_RESULT_DEADLINE;
                match resolve_unknown_cas(&svc.storage, &new_head, &expected, entry.id, until).await
                {
                    Resolution::Accepted(e) => e,
                    Resolution::Rejected => {
                        let _ = txn.abort().await;
                        return Ok(Attempt::Retry("unknown CAS resolved as rejected"));
                    }
                    Resolution::Unknown(m) => {
                        svc.metrics.inc("push_result_unknown");
                        let _ = txn.abort().await;
                        tracing::warn!(repo = %self.repo, request = %req.id, error = %m, "push result unknown");
                        // The next storage check applies the entry if it was accepted.
                        return Ok(Attempt::Done(reject_all(
                            names,
                            &format!(
                                "result unknown (request {}; storage did not answer: {m}); \
                                 fetch to check before retrying",
                                req.id
                            ),
                        )));
                    }
                }
            }
        };

        // Step 17: complete the local transaction. The push is accepted now,
        // whatever happens locally.
        svc.metrics.inc("pushes_accepted");
        tracing::info!(repo = %self.repo, request = %req.id, generation = entry.generation, refs = req.updates.len(), "push accepted");
        match txn.commit().await {
            Ok(()) => {
                let marker = super::sync::write_marker(
                    dir,
                    self.repo,
                    head.incarnation,
                    entry.generation,
                    entry.id,
                )
                .await;
                if marker.is_err() {
                    st.poison();
                } else {
                    self.set_known(
                        &mut st,
                        Known {
                            head: new_head.clone(),
                            entry: entry.id,
                            etag: accepted_etag,
                            refs: next,
                        },
                    );
                }
            }
            Err(e) => {
                tracing::error!(repo = %self.repo, request = %req.id, error = %e, "local completion failed after CAS; cache blocked");
                svc.metrics.inc("local_completion_failures");
                st.poison();
            }
        }
        drop(st);
        let receipt = Receipt {
            format: FORMAT_VERSION,
            repository: self.repo,
            incarnation: head.incarnation,
            request: req.id,
            request_checksum: req.checksum.clone(),
            entry: entry.id,
            generation: entry.generation,
        };
        if let Err(e) = svc.storage.put_receipt(&receipt).await {
            tracing::warn!(repo = %self.repo, request = %req.id, error = %e, "receipt not stored; the history still holds the entry");
        }
        let mut messages = vec![format!(
            "worktree: accepted as generation {} (request {})",
            entry.generation, req.id
        )];
        let mut generation = entry.generation;
        if let Some(branch) = adopt {
            match worktree_core::admin::set_default_branch(
                &svc.storage,
                self.repo,
                branch.clone(),
                &op.user,
            )
            .await
            {
                Ok(h) => {
                    generation = h.generation;
                    let short = branch.as_str().trim_start_matches("refs/heads/");
                    messages.push(format!("worktree: default branch is now {short}"));
                    if let Some(changed) = &op.default_branch_changed {
                        changed(self.repo, branch).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(repo = %self.repo, request = %req.id, error = %e, "default branch not changed")
                }
            }
        }
        self.pushed_here.store(true, Ordering::SeqCst);
        svc.bus.publish(self.repo, generation);
        Ok(Attempt::Done(HookResponse {
            results: names
                .iter()
                .map(|n| HookResult {
                    name: n.clone(),
                    ok: true,
                    message: None,
                })
                .collect(),
            messages,
        }))
    }

    /// Checks objects, types, sizes, fast-forward rules, and ref permissions.
    async fn check_objects_and_rules(
        &self,
        dir: &std::path::Path,
        base: &RefState,
        updates: &[RefUpdate],
        access: &PushAccess,
    ) -> Result<Option<Vec<(String, String)>>, RepoError> {
        let git = &self.svc.git;
        let new_oids: Vec<Oid> = updates.iter().filter_map(|u| u.new.clone()).collect();
        let types = git.object_types(dir, &new_oids).await?;
        let mut bad = Vec::new();
        for u in updates {
            let name = u.name.to_string();
            let rule = ref_rule(&access.ref_rules, u.name.as_str());
            let (kind, need) = match (&u.old, &u.new) {
                // Only the server's own changes get here with these.
                _ if u.name.is_pull() => ("update", Access::None),
                (_, None) => ("deletion", rule.map_or(Access::Write, |r| r.delete)),
                (None, Some(_)) => ("creation", rule.map_or(Access::Write, |r| r.update)),
                (Some(old), Some(new)) => {
                    let forced = match types.get(new).cloned().flatten() {
                        Some(_) if u.name.is_tag() => true,
                        Some(t) if t == "commit" => !git.is_ancestor(dir, old, new).await?,
                        // Missing or wrong objects are reported below.
                        _ => false,
                    };
                    if forced {
                        ("force", rule.map_or(Access::Force, |r| r.force))
                    } else {
                        ("update", rule.map_or(Access::Write, |r| r.update))
                    }
                }
            };
            if access.level < need {
                let why = match (kind, rule) {
                    ("force", None) if u.name.is_tag() => {
                        "tag update needs force permission".into()
                    }
                    ("force", None) => "non-fast-forward; force permission required".into(),
                    _ => format!("ref rule: {kind} needs {need:?} access").to_lowercase(),
                };
                bad.push((name, why));
                continue;
            }
            let Some(new) = &u.new else { continue };
            match types.get(new).cloned().flatten() {
                None => bad.push((name, "missing object".into())),
                Some(t) if (u.name.is_branch() || u.name.is_pull()) && t != "commit" => {
                    bad.push((name, format!("branch must point to a commit, not a {t}")))
                }
                Some(_) => {}
            }
        }
        if !bad.is_empty() {
            return Ok(Some(bad));
        }
        if !new_oids.is_empty() {
            let exclude: Vec<Oid> = base.refs.values().cloned().collect();
            let (total, largest) = git.object_sizes(dir, &new_oids, &exclude).await?;
            let reason = if largest > limits::MAX_OBJECT_BYTES {
                Some(format!(
                    "object larger than {} bytes",
                    limits::MAX_OBJECT_BYTES
                ))
            } else if total > limits::MAX_EXPANDED_BYTES {
                Some(format!(
                    "push expands to more than {} bytes",
                    limits::MAX_EXPANDED_BYTES
                ))
            } else {
                None
            };
            if let Some(r) = reason {
                self.svc.metrics.inc("object_limit_rejections");
                return Ok(Some(
                    updates
                        .iter()
                        .map(|u| (u.name.to_string(), r.clone()))
                        .collect(),
                ));
            }
        }
        Ok(None)
    }

    async fn build_and_store_pack(
        &self,
        dir: &std::path::Path,
        base: &RefState,
        updates: &[RefUpdate],
    ) -> Result<Option<PackRef>, RepoError> {
        let include: Vec<Oid> = updates.iter().filter_map(|u| u.new.clone()).collect();
        if include.is_empty() {
            return Ok(None);
        }
        let exclude: Vec<Oid> = base.refs.values().cloned().collect();
        let file = self
            .svc
            .tmp_dir()
            .join(format!("push-{}.pack", uuid::Uuid::new_v4().simple()));
        let result = async {
            let count = {
                let _t = StepTimer::new(&self.svc.metrics, "push_pack");
                self.svc
                    .git
                    .pack_objects(dir, &include, &exclude, &file)
                    .await?
            };
            if count == 0 {
                return Ok(None);
            }
            let _t = StepTimer::new(&self.svc.metrics, "push_upload");
            Ok(Some(self.svc.storage.store_pack(self.repo, &file).await?))
        }
        .await;
        let _ = tokio::fs::remove_file(&file).await;
        result
    }
}

fn same_cache(current: &Option<Arc<CacheInstance>>, used: &Arc<CacheInstance>) -> bool {
    current
        .as_ref()
        .map(|c| Arc::ptr_eq(c, used))
        .unwrap_or(false)
}

fn parse_commands(
    format: ObjectFormat,
    commands: &[HookCommand],
) -> Result<Vec<RefUpdate>, String> {
    if commands.is_empty() {
        return Err("no commands".into());
    }
    commands
        .iter()
        .map(|c| {
            Ok(RefUpdate {
                name: RefName::parse(&c.name).map_err(|e| e.to_string())?,
                old: Oid::parse_git(format, &c.old).map_err(|e| e.to_string())?,
                new: Oid::parse_git(format, &c.new).map_err(|e| e.to_string())?,
            })
        })
        .collect()
}

/// The branch that becomes the default after a push into an empty
/// repository whose default branch the push did not create: `main`, then
/// `master`, then the first pushed branch by name.
fn adopted_default_branch(base: &RefState, next: &RefState) -> Option<RefName> {
    if !base.refs.is_empty()
        || next
            .head
            .as_ref()
            .is_some_and(|h| next.refs.contains_key(h))
    {
        return None;
    }
    let branches: Vec<&RefName> = next.refs.keys().filter(|r| r.is_branch()).collect();
    ["refs/heads/main", "refs/heads/master"]
        .iter()
        .find_map(|p| branches.iter().find(|b| b.as_str() == *p))
        .or(branches.first())
        .map(|b| (*b).clone())
}

fn reject_all(names: &[String], reason: &str) -> HookResponse {
    HookResponse {
        results: names
            .iter()
            .map(|n| HookResult {
                name: n.clone(),
                ok: false,
                message: Some(reason.into()),
            })
            .collect(),
        messages: vec![],
    }
}

/// Rejects the complete push; failing refs get their own reason.
fn reject_some(names: &[String], failed: &[(String, String)]) -> HookResponse {
    HookResponse {
        results: names
            .iter()
            .map(|n| {
                let reason = failed
                    .iter()
                    .find(|(f, _)| f == n)
                    .map(|(_, r)| r.clone())
                    .unwrap_or_else(|| "atomic push rejected".into());
                HookResult {
                    name: n.clone(),
                    ok: false,
                    message: Some(reason),
                }
            })
            .collect(),
        messages: vec![],
    }
}
