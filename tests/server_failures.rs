//! Server failure (IMPLEMENTATION_PLAN.md test group): a server stops at
//! each step of the push procedure. The head CAS decides the result: before
//! it, the previous state remains; after it, the push is retained. A server
//! restarted on the same cache directory, and one with an empty cache, serve
//! exactly the stored state.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use rustgit::failpoint::{self, FailAction};

async fn stop_during_push(point: &str) {
    let env = Env::new();
    let server = env.start_with(|_| {}, "stopped").await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await;

    let c2 = alice
        .commit(&a, &[("g", &noise(5, 50_000)[..])], "two")
        .await;
    let reached = Arc::new(tokio::sync::Notify::new());
    server
        .svc()
        .failpoints
        .set(point, FailAction::Pause(reached.clone()));
    let push = tokio::spawn({
        let (home, dir) = (alice.home.clone(), a.clone());
        async move { push_result(&Client { home }, &dir, &["push", "origin", "main"]).await }
    });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .unwrap_or_else(|_| panic!("{point}: the push reaches the failpoint"));
    server.running.stop();
    let result = tokio::time::timeout(Duration::from_secs(30), push)
        .await
        .expect("the client ends when the server stops")
        .unwrap();
    // The client never saw success, whatever storage decided.
    assert_ne!(result, PushResult::Accepted, "{point}");

    let accepted = matches!(
        point,
        failpoint::PUSH_AFTER_CAS | failpoint::PUSH_AFTER_LOCAL_COMMIT
    );
    let head = env.head().await;
    let expected_main = if accepted {
        assert_eq!(head.generation, before.generation + 1, "{point}");
        &c2
    } else {
        assert_eq!(head.revision, before.revision, "{point}: head unchanged");
        &c1
    };
    let stored = env.stored_refs().await;
    assert_eq!(&stored["refs/heads/main"], expected_main, "{point}");

    // A restart on the same cache directory, and a server with an empty cache.
    let restarted = env.start_with(|_| {}, "stopped").await;
    let empty = env.start().await;
    for s in [&restarted, &empty] {
        assert_eq!(alice.ls_remote(&s.url()).await, stored, "{point}");
    }
    // The cache holds a ref transaction that did not finish, or the push is
    // accepted in storage but not in the cache: the cache is rebuilt.
    let rebuilt = matches!(
        point,
        failpoint::PUSH_AFTER_PREPARE | failpoint::PUSH_AFTER_CAS
    );
    assert_eq!(
        restarted.metric("cache_rebuilds"),
        rebuilt as u64,
        "{point}: rebuild of the restarted cache"
    );
    let bob = env.client("bob");
    let b = bob.clone(&restarted.url_as("bob"), "copy").await;
    assert_eq!(&bob.rev(&b, "HEAD").await, expected_main, "{point}");
    bob.ok(&b, &["fsck", "--full", "--strict"]).await;

    // The client can repeat the push, and later pushes work.
    alice
        .ok(&a, &["push", "--quiet", &restarted.url(), "main"])
        .await;
    let c3 = alice.commit(&a, &[("f", b"3")], "three").await;
    alice
        .ok(&a, &["push", "--quiet", &restarted.url(), "main"])
        .await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], c3);
    check_history(
        &env,
        &[(c2, PushResult::Unknown), (c3, PushResult::Accepted)],
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_before_the_pack_upload_keeps_the_previous_state() {
    stop_during_push(failpoint::PUSH_BEFORE_UPLOAD).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_after_the_pack_upload_keeps_the_previous_state() {
    stop_during_push(failpoint::PUSH_AFTER_UPLOAD).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_after_the_entry_upload_keeps_the_previous_state() {
    stop_during_push(failpoint::PUSH_AFTER_ENTRY).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_after_the_ref_preparation_keeps_the_previous_state() {
    stop_during_push(failpoint::PUSH_AFTER_PREPARE).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_after_the_cas_retains_the_push() {
    stop_during_push(failpoint::PUSH_AFTER_CAS).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_after_the_local_installation_retains_the_push() {
    stop_during_push(failpoint::PUSH_AFTER_LOCAL_COMMIT).await;
}

/// A server stops while it rebuilds a cache. The partial copy is never
/// served, and the next start removes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_during_cache_reconstruction_leaves_no_partial_cache() {
    let env = Env::new();
    let first = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&first.url(), "work").await;
    let tip = alice.commit(&a, &[("f", &text(200, "x")[..])], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let rebuilding = env.start_with(|_| {}, "rebuilding").await;
    let reached = Arc::new(tokio::sync::Notify::new());
    rebuilding.svc().failpoints.set(
        failpoint::REBUILD_BEFORE_REPLACE,
        FailAction::Pause(reached.clone()),
    );
    let read = tokio::spawn({
        let (home, url) = (alice.home.clone(), rebuilding.url());
        async move {
            Client { home }
                .run(std::path::Path::new("/"), &["ls-remote", &url])
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .unwrap();
    let cache_dir = rebuilding
        .cache_dir
        .join("repos")
        .join(env.repo.to_string());
    let partial: Vec<_> = std::fs::read_dir(&cache_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(
        partial
            .iter()
            .all(|p| p.extension().is_some_and(|x| x == "tmp")),
        "only a temporary directory exists: {partial:?}"
    );
    rebuilding.running.stop();
    let out = tokio::time::timeout(Duration::from_secs(30), read)
        .await
        .unwrap()
        .unwrap();
    assert!(!out.status.success(), "no answer from a partial cache");

    let restarted = env.start_with(|_| {}, "rebuilding").await;
    assert_eq!(
        alice.ls_remote(&restarted.url()).await["refs/heads/main"],
        tip
    );
    let left: Vec<_> = std::fs::read_dir(&cache_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(
        left.iter().all(|p| !partial.contains(p)),
        "the partial copy was removed: {left:?}"
    );
}
