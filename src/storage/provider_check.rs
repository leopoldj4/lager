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
    // 6. Concurrent writers: exactly one wins, whichever service nodes
    // the requests reach. A provider that checks and then writes without a
    // lock lets two pushes accept the same head.
    let race_key = format!("{}race.json", storage.repo_prefix(repo));
    out.push((
        "concurrent conditional creates: exactly one succeeds",
        race(&b, &race_key, 0, WriteMode::Create).await,
    ));
    let mut cas_race = Ok(());
    for round in 1..=RACE_ROUNDS {
        let etag = match b.get(&race_key, None).await {
            Ok(f) => f.etag,
            Err(e) => {
                cas_race = Err(e.to_string());
                break;
            }
        };
        let Some(etag) = etag else {
            cas_race = Err("read returned no ETag".into());
            break;
        };
        cas_race = race(&b, &race_key, round, WriteMode::Update(etag)).await;
        if cas_race.is_err() {
            break;
        }
    }
    out.push((
        "concurrent CAS: exactly one writer wins each round",
        cas_race.map_err(|e| format!("{e} (after the rounds before it passed)")),
    ));
    cleanup(storage, repo).await;
    out
}

const RACE_WRITERS: usize = 8;
const RACE_ROUNDS: usize = 5;

/// Starts `RACE_WRITERS` conditional writes of different contents at once.
/// Exactly one must succeed, the others must report a conflict, and a read
/// must return the winner's contents.
async fn race(
    b: &std::sync::Arc<dyn super::Backend>,
    key: &str,
    round: usize,
    mode: WriteMode,
) -> CheckResult {
    let contents = |i: usize| Bytes::from(format!("round {round} writer {i}"));
    let writes = (0..RACE_WRITERS).map(|i| b.put(key, contents(i), mode.clone()));
    let results = futures::future::join_all(writes).await;
    let mut winners = Vec::new();
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok(_) => winners.push(i),
            Err(BackendError::Conflict) => {}
            Err(e) => return Err(format!("round {round}: writer {i}: {e}")),
        }
    }
    let [winner] = winners[..] else {
        return Err(format!(
            "round {round}: {} of {RACE_WRITERS} writers succeeded",
            winners.len()
        ));
    };
    match b.get(key, None).await {
        Ok(f) if f.data == contents(winner) => Ok(()),
        Ok(f) => Err(format!(
            "round {round}: writer {winner} succeeded, but a read returns {:?}",
            String::from_utf8_lossy(&f.data)
        )),
        Err(e) => Err(format!("round {round}: {e}")),
    }
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
