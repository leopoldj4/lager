//! Maintenance (IMPLEMENTATION_PLAN.md test group): competing compactors,
//! new pushes, interrupted uploads, damaged snapshots, missing packs, the
//! data removal fence, and automatic snapshots.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use rustgit::maintenance::{CleanupOptions, cleanup, snapshot};
use rustgit::model::Operation;
use rustgit::storage::WriteMode;
use rustgit::storage::fault::{Action, Op};

async fn push_n(client: &Client, dir: &std::path::Path, n: usize, tag: &str) -> String {
    let mut last = String::new();
    for i in 0..n {
        last = client
            .commit(dir, &[(&format!("{tag}{i}"), &text(10, tag)[..])], tag)
            .await;
        client.ok(dir, &["push", "--quiet", "origin", "HEAD"]).await;
    }
    last
}

/// Two servers compact the same repository while a client pushes. A
/// compaction either attaches its snapshot or finds a newer one attached; no
/// push is lost, and recovery matches the log.
#[tokio::test(flavor = "multi_thread")]
async fn competing_compactors_keep_every_accepted_push() {
    let env = Env::new();
    let servers = [env.start().await, env.start().await];
    let alice = env.client("alice");
    let a = alice.init(&servers[0].url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    push_n(&alice, &a, 3, "before").await;

    let c0 = servers[0].running.registry().controller(env.repo);
    let c1 = servers[1].running.registry().controller(env.repo);
    let (r0, r1, pushed) = tokio::join!(
        snapshot(&c0, "m0"),
        snapshot(&c1, "m1"),
        push_n(&alice, &a, 2, "during"),
    );
    let attached: Vec<_> = [r0, r1]
        .into_iter()
        .filter_map(|r| match r {
            Ok(r) => Some(r),
            Err(e) => {
                assert!(e.to_string().contains("newer snapshot"), "{e}");
                None
            }
        })
        .collect();
    assert!(!attached.is_empty());
    if let [a, b] = &attached[..] {
        assert_ne!(a.generation, b.generation, "each attached its own entry");
    }
    let newest = attached.iter().map(|r| r.base_generation).max().unwrap();
    assert_eq!(
        env.head().await.snapshot.unwrap().base_generation,
        newest,
        "the newest snapshot is attached"
    );
    assert_eq!(env.stored_refs().await["refs/heads/main"], pushed);
    check_history(&env, &[(pushed.clone(), PushResult::Accepted)]).await;

    let fresh = env.start().await;
    let bob = env.client("bob");
    let b = bob.clone(&fresh.url_as("bob"), "copy").await;
    assert_eq!(bob.rev(&b, "HEAD").await, pushed);
    bob.ok(&b, &["fsck", "--full", "--strict"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_snapshot_upload_leaves_the_head_unchanged() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", &noise(1, 20_000)[..])], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await;

    env.fault
        .add_rule(Op::PutFile, "/packs/", Action::Fail, None);
    let controller = server.running.registry().controller(env.repo);
    assert!(snapshot(&controller, "test").await.is_err());
    env.fault.clear_rules();
    let after = env.head().await;
    assert_eq!(after.revision, before.revision);
    assert!(after.snapshot.is_none());

    snapshot(&controller, "test").await.unwrap();
    assert!(env.head().await.snapshot.is_some());
}

/// A damaged snapshot record, or one whose refs name missing objects, is
/// detected during recovery. Nothing partial is served.
#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_snapshot_is_detected_and_not_served() {
    for damage in ["not a record", "missing object"] {
        let env = Env::new();
        let server = env.start().await;
        let alice = env.client("alice");
        let a = alice.init(&server.url(), "work").await;
        alice.commit(&a, &[("f", b"0")], "0").await;
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
        let controller = server.running.registry().controller(env.repo);
        snapshot(&controller, "test").await.unwrap();
        let id = env.head().await.snapshot.unwrap().id;
        let key = env.storage.snapshot_key(env.repo, id);
        let backend = env.storage.backend();
        let current = backend.get(&key, None).await.unwrap();
        let data = if damage == "not a record" {
            b"{ damaged".to_vec()
        } else {
            let mut s: serde_json::Value = serde_json::from_slice(&current.data).unwrap();
            s["refs"]["refs/heads/main"] = "1111111111111111111111111111111111111111".into();
            serde_json::to_vec(&s).unwrap()
        };
        backend
            .put(&key, data.into(), WriteMode::Update(current.etag.unwrap()))
            .await
            .unwrap();

        let fresh = env.start().await;
        let bob = env.client("bob");
        let err = bob
            .fails(&bob.home, &["clone", &fresh.url_as("bob"), "copy"])
            .await;
        assert!(err.contains("500"), "{damage}: {err}");
        assert!(!bob.home.join("copy").exists());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_pack_is_detected_and_not_served() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", &noise(4, 5000)[..])], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    alice.commit(&a, &[("g", &noise(5, 5000)[..])], "1").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let pack = env.push_entries().await[0].packs[0].clone();
    env.storage
        .delete(&env.storage.pack_key(env.repo, &pack.sha256))
        .await
        .unwrap();

    let fresh = env.start().await;
    let bob = env.client("bob");
    let err = bob
        .fails(&bob.home, &["ls-remote", &fresh.url_as("bob")])
        .await;
    assert!(err.contains("500"), "{err}");
    // A server with a complete cache still serves.
    assert_eq!(alice.ls_remote(&server.url()).await.len(), 1);
}

fn short_fence() -> CleanupOptions {
    CleanupOptions {
        removal_window: Duration::from_secs(1),
        clock_margin: Duration::ZERO,
        ..CleanupOptions::remove(Duration::ZERO)
    }
}

/// A push waits for its CAS while data removal runs with a retention period
/// of zero, so the push's own upload is removed. The fence makes the waiting
/// CAS fail; the retry cannot use the removed pack while the fence is active.
#[tokio::test(flavor = "multi_thread")]
async fn the_removal_fence_stops_a_delayed_push_from_using_removed_data() {
    for release_after_fence in [false, true] {
        let env = Env::new();
        let server = env.start().await;
        let alice = env.client("alice");
        let a = alice.init(&server.url(), "work").await;
        alice.commit(&a, &[("f", b"0")], "0").await;
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

        let c2 = alice
            .commit(&a, &[("g", &noise(6, 30_000)[..])], "delayed")
            .await;
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        env.fault.add_rule(
            Op::Put,
            "head.json",
            Action::Gate(reached.clone(), release.clone()),
            Some(1),
        );
        let push = tokio::spawn({
            let (home, dir) = (alice.home.clone(), a.clone());
            async move { push_result(&Client { home }, &dir, &["push", "origin", "main"]).await }
        });
        tokio::time::timeout(Duration::from_secs(30), reached.notified())
            .await
            .unwrap();
        let report = cleanup(&env.storage, env.repo, &short_fence())
            .await
            .unwrap();
        assert!(report.fence_generation.is_some());
        assert!(
            report.candidates.iter().any(|c| c.key.contains("/packs/")),
            "the delayed push's pack was removed: {:#?}",
            report.candidates
        );
        assert!(report.removed >= 1);
        if release_after_fence {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        release.notify_one();
        let result = push.await.unwrap();
        let history_has_c2 = env
            .history()
            .await
            .iter()
            .flat_map(|e| &e.refs)
            .any(|u| u.new.as_ref().map(|o| o.as_str()) == Some(c2.as_str()));
        if release_after_fence {
            // After the fence, the retry uploads its pack again.
            assert_eq!(result, PushResult::Accepted);
            assert_eq!(server.metric("push_removal_fence_waits"), 0);
            assert!(
                server.metric("cas_conflicts") >= 1,
                "the fence stopped the waiting CAS"
            );
            assert!(history_has_c2);
        } else {
            assert_eq!(result, PushResult::Rejected);
            assert_eq!(server.metric("push_removal_fence_waits"), 1);
            assert!(!history_has_c2);
            tokio::time::sleep(Duration::from_millis(1200)).await;
            alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
        }
        // Every accepted change is recoverable from storage.
        let fresh = env.start().await;
        let bob = env.client("bob");
        let b = bob.clone(&fresh.url_as("bob"), "copy").await;
        assert_eq!(bob.rev(&b, "HEAD").await, c2);
        assert_eq!(std::fs::read(b.join("g")).unwrap(), noise(6, 30_000));
    }
}

/// Packs and records have separate retention periods.
#[tokio::test(flavor = "multi_thread")]
async fn packs_and_records_have_separate_retention_periods() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    push_n(&alice, &a, 2, "x").await;
    let controller = server.running.registry().controller(env.repo);
    snapshot(&controller, "test").await.unwrap();

    let opts = CleanupOptions {
        pack_retention: Duration::ZERO,
        record_retention: Duration::from_secs(3600),
        ..CleanupOptions::report(Duration::ZERO)
    };
    let report = cleanup(&env.storage, env.repo, &opts).await.unwrap();
    assert!(!report.candidates.is_empty());
    assert!(report.candidates.iter().all(|c| c.key.contains("/packs/")));
    assert!(report.retained_recent >= 3, "the receipts are kept");
    let opts = CleanupOptions {
        pack_retention: Duration::from_secs(3600),
        record_retention: Duration::ZERO,
        ..opts
    };
    let report = cleanup(&env.storage, env.repo, &opts).await.unwrap();
    let count = |kind: &str| {
        report
            .candidates
            .iter()
            .filter(|c| c.key.contains(kind))
            .count()
    };
    // The receipts, and the entries that the snapshot folded: the creation
    // entry and the three pushes.
    assert_eq!(count("/receipts/"), 3);
    assert_eq!(count("/entries/"), 4);
    assert_eq!(report.candidates.len(), 7);
}

/// Only the preferred server of a repository starts automatic snapshots.
#[tokio::test(flavor = "multi_thread")]
async fn the_preferred_server_creates_snapshots_automatically() {
    let env = Env::new();
    let servers = env
        .cluster(2, |cfg| {
            cfg.maintenance.snapshot_after_entries = 3;
            cfg.maintenance.check_interval_secs = 1;
        })
        .await;
    let alice = env.client("alice");
    let bob = env.client("bob");
    // Both servers have a cache of the repository.
    let a = alice.init(&servers[0].url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let tip = push_n(&alice, &a, 3, "x").await;
    bob.clone(&servers[1].url_as("bob"), "copy").await;

    let start = Instant::now();
    while env.head().await.snapshot.is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "snapshot created"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let preferred = rustgit::routing::rank(env.repo, ["s0", "s1"])[0];
    for (i, s) in servers.iter().enumerate() {
        let expected = (format!("s{i}") == preferred) as u64;
        assert_eq!(s.metric("snapshots"), expected, "s{i}");
    }
    // The snapshot can come before the last push; it never replaces it.
    let snapshots: Vec<_> = env
        .history()
        .await
        .into_iter()
        .filter(|e| matches!(e.operation, Operation::Snapshot { .. }))
        .collect();
    assert!(!snapshots.is_empty());
    assert!(snapshots.iter().all(|e| e.user == "rustgit-maintenance"));
    check_history(&env, &[(tip.clone(), PushResult::Accepted)]).await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], tip);
}
