//! Checks the storage requirements (ARCHITECTURE.md §5) against a provider.
//!
//! S3 compatibility alone is insufficient: run these checks against every
//! provider before use, and repeat them before each release.

use bytes::Bytes;

use super::Storage;
use super::backend::{BackendError, WriteMode};
use crate::model::RepoId;

type CheckResult = Result<(), String>;

/// Runs all checks under a new random repository prefix and removes the
/// test objects afterwards.
pub async fn run(storage: &Storage) -> Vec<(&'static str, CheckResult)> {
    let repo = RepoId::new();
    let key = storage.head_key(repo);
    let b = storage.backend().clone();
    let mut out: Vec<(&'static str, CheckResult)> = Vec::new();

    // 2. Create only if absent.
    let first = b
        .put(&key, Bytes::from_static(b"one"), WriteMode::Create)
        .await;
    let etag1 = match &first {
        Ok(Some(e)) => Some(e.clone()),
        _ => None,
    };
    out.push(("conditional create of a new object", expect_ok(&first)));
    let second = b
        .put(&key, Bytes::from_static(b"two"), WriteMode::Create)
        .await;
    out.push((
        "conditional create of an existing object reports a conflict",
        expect_conflict(&second),
    ));

    // 1. Current read after an accepted change.
    let read = b.get(&key, None).await;
    out.push((
        "read returns the accepted contents and ETag",
        match &read {
            Ok(f) if &f.data[..] == b"one" && f.etag.is_some() && f.etag == etag1 => Ok(()),
            Ok(f) => Err(format!(
                "got {:?} etag {:?}, expected etag {:?}",
                f.data, f.etag, etag1
            )),
            Err(e) => Err(e.to_string()),
        },
    ));
    let Some(etag1) = read.ok().and_then(|f| f.etag) else {
        cleanup(storage, repo).await;
        return out;
    };

    // 3. Replace only if the ETag matches; 5. conflicts are distinct errors.
    let wrong = b
        .put(
            &key,
            Bytes::from_static(b"x"),
            WriteMode::Update("\"not-the-etag\"".into()),
        )
        .await;
    out.push((
        "CAS with a wrong ETag reports a conflict",
        expect_conflict(&wrong),
    ));
    let cas = b
        .put(
            &key,
            Bytes::from_static(b"two"),
            WriteMode::Update(etag1.clone()),
        )
        .await;
    out.push(("CAS with the current ETag succeeds", expect_ok(&cas)));
    let stale = b
        .put(
            &key,
            Bytes::from_static(b"three"),
            WriteMode::Update(etag1.clone()),
        )
        .await;
    out.push((
        "CAS with a replaced ETag reports a conflict",
        expect_conflict(&stale),
    ));
    let after = b.get(&key, None).await;
    out.push((
        "read after CAS returns the new contents",
        match &after {
            Ok(f) if &f.data[..] == b"two" => Ok(()),
            Ok(f) => Err(format!("got {:?}", f.data)),
            Err(e) => Err(e.to_string()),
        },
    ));
    let etag2 = after.ok().and_then(|f| f.etag);
    if let Some(e2) = &etag2 {
        let unchanged = b.get(&key, Some(e2)).await;
        out.push((
            "conditional read reports an unchanged object",
            match unchanged {
                Err(BackendError::NotModified) => Ok(()),
                other => Err(format!("{:?}", other.map(|f| f.data))),
            },
        ));
        let changed = b.get(&key, Some(&etag1)).await;
        out.push((
            "conditional read returns a changed object",
            match changed {
                Ok(f) if &f.data[..] == b"two" => Ok(()),
                other => Err(format!("{:?}", other.map(|f| f.data))),
            },
        ));
    }
    // Same contents written again must give a different head: the head
    // carries a revision UUID so that equal ETags cannot hide a change.
    // 4. Complete files without later changes.
    let rec_key = format!("{}entries/check.json", storage.repo_prefix(repo));
    let r1 = storage
        .put_record(&rec_key, &serde_json::json!({"a": 1}))
        .await;
    let r2 = storage
        .put_record(&rec_key, &serde_json::json!({"a": 1}))
        .await;
    let r3 = storage
        .put_record(&rec_key, &serde_json::json!({"a": 2}))
        .await;
    out.push((
        "immutable records accept identical repeats and refuse changes",
        match (r1, r2, r3) {
            (Ok(()), Ok(()), Err(_)) => Ok(()),
            other => Err(format!("{other:?}")),
        },
    ));
    let missing = b
        .get(&format!("{}missing", storage.repo_prefix(repo)), None)
        .await;
    out.push((
        "missing objects are reported as missing",
        match missing {
            Err(BackendError::NotFound) => Ok(()),
            other => Err(format!("{:?}", other.map(|f| f.data))),
        },
    ));
    cleanup(storage, repo).await;
    out
}

async fn cleanup(storage: &Storage, repo: RepoId) {
    if let Ok(list) = storage.list_repo(repo).await {
        for o in list {
            let _ = storage.delete(&o.key).await;
        }
    }
}

fn expect_ok<T>(r: &Result<T, BackendError>) -> CheckResult {
    r.as_ref().map(|_| ()).map_err(|e| e.to_string())
}

fn expect_conflict<T>(r: &Result<T, BackendError>) -> CheckResult {
    match r {
        Err(BackendError::Conflict) => Ok(()),
        Err(e) => Err(format!("expected a conflict, got {e}")),
        Ok(_) => Err("expected a conflict, the write succeeded".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::storage::ObjectStoreBackend;

    #[tokio::test]
    async fn in_memory_store_meets_the_storage_requirements() {
        let s = Storage::new(Arc::new(ObjectStoreBackend::in_memory()), "t");
        for (name, r) in run(&s).await {
            assert!(r.is_ok(), "{name}: {r:?}");
        }
    }
}
