//! Stage 1 checks (IMPLEMENTATION_PLAN.md): one server, standard Git clients.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use rustgit::model::Operation;
use rustgit::storage::fault::{Action, Op};

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_exchange_changes_through_the_server() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let bob = env.client("bob");

    let a = alice.init(&server.url_as("alice"), "work").await;
    let c1 = alice
        .commit(&a, &[("readme.txt", b"from alice\n")], "first")
        .await;
    alice
        .ok(&a, &["push", "--quiet", "-u", "origin", "main"])
        .await;

    let b = bob.clone(&server.url_as("bob"), "work").await;
    assert_eq!(bob.rev(&b, "HEAD").await, c1);
    assert_eq!(
        std::fs::read(b.join("readme.txt")).unwrap(),
        b"from alice\n"
    );
    let c2 = bob
        .commit(&b, &[("bob.txt", b"from bob\n")], "second")
        .await;
    bob.ok(&b, &["push", "--quiet", "origin", "main"]).await;

    alice.ok(&a, &["pull", "--quiet", "--ff-only"]).await;
    assert_eq!(alice.rev(&a, "HEAD").await, c2);
    assert_eq!(std::fs::read(a.join("bob.txt")).unwrap(), b"from bob\n");
    assert_eq!(
        env.stored_refs().await,
        [("refs/heads/main".to_string(), c2)].into()
    );
}

/// The client must not see success while the head CAS waits in storage.
#[tokio::test(flavor = "multi_thread")]
async fn push_success_is_reported_only_after_storage_accepts_the_head() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await.generation;

    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    env.fault.add_rule(
        Op::Put,
        "head.json",
        Action::Gate(reached.clone(), release.clone()),
        Some(1),
    );
    let mut push = alice
        .command(&a)
        .args(["push", "--quiet", "origin", "main"])
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .expect("push reaches the head CAS");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        push.try_wait().unwrap().is_none(),
        "the client finished before the CAS"
    );
    assert_eq!(env.head().await.generation, before);

    release.notify_one();
    let status = tokio::time::timeout(Duration::from_secs(30), push.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert_eq!(env.head().await.generation, before + 1);
    assert_eq!(env.stored_refs().await["refs/heads/main"], c2);
}

/// Small pushes arrive as loose objects, large ones as thin packs. Both
/// must be in storage: a server with an empty cache serves the same data.
#[tokio::test(flavor = "multi_thread")]
async fn loose_objects_and_thin_packs_survive_complete_cache_loss() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;

    // 150 files: more objects than receive.unpackLimit, so Git keeps a pack.
    let big = text(4000, "big");
    let mut files: Vec<(String, Vec<u8>)> = vec![("big.txt".into(), big.clone())];
    for i in 0..150 {
        files.push((format!("dir/{i}.txt"), text(3, &format!("small {i}"))));
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), &d[..])).collect();
    alice.commit(&a, &refs, "many files").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    // A thin pack: the changed big file is a delta against the stored one.
    let mut big2 = big.clone();
    big2.extend_from_slice(b"one more line\n");
    let mut files2: Vec<(String, Vec<u8>)> = vec![("big.txt".into(), big2.clone())];
    for i in 0..120 {
        files2.push((format!("more/{i}.txt"), text(2, &format!("more {i}"))));
    }
    let refs2: Vec<(&str, &[u8])> = files2.iter().map(|(n, d)| (n.as_str(), &d[..])).collect();
    alice.commit(&a, &refs2, "thin").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    // Loose objects: one small commit.
    let tip = alice
        .commit(&a, &[("loose.txt", b"loose\n")], "loose")
        .await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let pushes = env.push_entries().await;
    assert_eq!(pushes.len(), 3);
    assert!(
        pushes.iter().all(|e| e.packs.len() == 1),
        "each push stores a pack"
    );

    let expected = tree_files(&alice, &a, "HEAD").await;
    server.stop_and_remove_cache();
    let fresh = env.start().await;
    let bob = env.client("bob");
    let b = bob.clone(&fresh.url_as("bob"), "copy").await;
    assert_eq!(bob.rev(&b, "HEAD").await, tip);
    assert_eq!(tree_files(&bob, &b, "HEAD").await, expected);
    bob.ok(&b, &["fsck", "--full", "--strict"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn annotated_tags_and_deletion_only_pushes_are_stored() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["tag", "-a", "v1", "-m", "release 1"]).await;
    alice.ok(&a, &["branch", "topic"]).await;
    alice
        .ok(&a, &["push", "--quiet", "origin", "main", "topic", "v1"])
        .await;
    let tag = alice.rev(&a, "v1").await;
    assert_ne!(tag, c1);

    let bob = env.client("bob");
    let b = bob.clone(&server.url_as("bob"), "copy").await;
    assert_eq!(bob.ok(&b, &["cat-file", "-t", "v1"]).await, "tag");
    assert_eq!(bob.rev(&b, "v1").await, tag);
    assert_eq!(bob.rev(&b, "v1^{commit}").await, c1);

    // Deleting a branch sends no objects.
    let entries_before = env.push_entries().await.len();
    alice.ok(&a, &["push", "--quiet", "origin", ":topic"]).await;
    let pushes = env.push_entries().await;
    assert_eq!(pushes.len(), entries_before + 1);
    let last = pushes.last().unwrap();
    assert!(last.packs.is_empty());
    assert_eq!(last.refs.len(), 1);
    assert!(last.refs[0].new.is_none());
    assert_eq!(
        env.stored_refs().await,
        [
            ("refs/heads/main".to_string(), c1.clone()),
            ("refs/tags/v1".to_string(), tag.clone())
        ]
        .into()
    );
    assert_eq!(env.stored_refs().await, bob.ls_remote(&server.url()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn ref_updates_that_need_no_new_objects_are_accepted() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    // Both commits already exist on the server.
    alice
        .ok(
            &a,
            &["push", "--quiet", "origin", &format!("{c1}:refs/heads/old")],
        )
        .await;
    alice.ok(&a, &["tag", "light", &c1]).await;
    alice.ok(&a, &["push", "--quiet", "origin", "light"]).await;

    let pushes = env.push_entries().await;
    assert_eq!(pushes.len(), 3);
    assert!(pushes[1].packs.is_empty() && pushes[2].packs.is_empty());
    assert_eq!(
        env.stored_refs().await,
        [
            ("refs/heads/main".to_string(), c2),
            ("refs/heads/old".to_string(), c1.clone()),
            ("refs/tags/light".to_string(), c1),
        ]
        .into()
    );
}

/// One invalid change rejects the complete push, atomic or not.
#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_ref_change_rejects_the_complete_push() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await;

    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    let blob = alice.ok(&a, &["rev-parse", "HEAD:f"]).await;
    for atomic in [true, false] {
        let mut args = vec!["push", "--porcelain", "origin"];
        if atomic {
            args.push("--atomic");
        }
        let blob_spec = format!("{blob}:refs/heads/blob");
        args.extend(["main", &blob_spec]);
        let out = alice.run(&a, &args).await;
        assert!(!out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("refs/heads/blob") && stdout.contains("must point to a commit")
                || atomic,
            "{stdout}"
        );
        assert!(
            !stdout
                .lines()
                .any(|l| l.starts_with("*") || l.starts_with(" ")),
            "{stdout}"
        );
    }
    let after = env.head().await;
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.revision, before.revision);
    assert_eq!(
        env.stored_refs().await,
        [("refs/heads/main".to_string(), c1)].into()
    );
    // The valid change alone succeeds afterwards.
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], c2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_atomic_push_of_several_refs_is_one_log_entry() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["branch", "b1"]).await;
    alice.ok(&a, &["branch", "b2"]).await;
    alice.ok(&a, &["tag", "t1"]).await;
    alice
        .ok(
            &a,
            &[
                "push", "--quiet", "--atomic", "origin", "main", "b1", "b2", "t1",
            ],
        )
        .await;
    let pushes = env.push_entries().await;
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].refs.len(), 4);
    let refs = env.stored_refs().await;
    assert_eq!(refs.len(), 4);
    assert!(refs.values().all(|v| *v == c1));
}

/// Ref names that conflict in the ref namespace are rejected.
#[tokio::test(flavor = "multi_thread")]
async fn conflicting_ref_names_are_rejected() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice
        .ok(
            &a,
            &["push", "--quiet", "origin", "main", "main:refs/heads/x"],
        )
        .await;
    alice
        .fails(&a, &["push", "--quiet", "origin", "main:refs/heads/x/y"])
        .await;
    assert_eq!(
        env.stored_refs().await,
        [
            ("refs/heads/main".to_string(), c1.clone()),
            ("refs/heads/x".to_string(), c1)
        ]
        .into()
    );
}

/// A lost CAS response: the write happened, the server did not hear about it.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_cas_response_does_not_duplicate_the_push() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await.generation;

    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    env.fault
        .add_rule(Op::Put, "head.json", Action::LoseResponse, Some(1));
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    assert_eq!(env.head().await.generation, before + 1);
    let pushes = env.push_entries().await;
    assert_eq!(pushes.len(), 2, "exactly one entry for the second push");
    assert_eq!(pushes[1].refs[0].new.as_ref().unwrap().as_str(), c2);
    assert!(
        env.fault
            .calls()
            .iter()
            .any(|c| c.key.ends_with("head.json") && c.outcome == "lost-response(ok)")
    );
    // The server's own view agrees with storage.
    assert_eq!(
        alice.ls_remote(&server.url()).await,
        env.stored_refs().await
    );
}

/// A failed CAS request without an answer: the identical request is repeated.
#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_cas_is_repeated_once_and_applied_once() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await.generation;

    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    env.fault
        .add_rule(Op::Put, "head.json", Action::Fail, Some(1));
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(env.head().await.generation, before + 1);
    assert_eq!(env.push_entries().await.len(), 2);
    assert_eq!(env.stored_refs().await["refs/heads/main"], c2);
}

/// Storage never answers: the push result stays unknown and the client is
/// not told that the push succeeded. Nothing becomes visible.
#[tokio::test(flavor = "multi_thread")]
async fn a_cas_that_never_answers_reports_no_success_and_keeps_the_head() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await;

    alice.commit(&a, &[("f", b"2")], "two").await;
    env.fault.add_rule(Op::Put, "head.json", Action::Fail, None);
    let err = alice.fails(&a, &["push", "origin", "main"]).await;
    assert!(err.contains("result unknown"), "{err}");
    env.fault.clear_rules();

    assert_eq!(env.head().await.revision, before.revision);
    assert_eq!(
        env.stored_refs().await,
        [("refs/heads/main".to_string(), c1.clone())].into()
    );
    assert_eq!(alice.ls_remote(&server.url()).await["refs/heads/main"], c1);
    // The push can be repeated.
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(env.head().await.generation, before.generation + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_pack_upload_keeps_the_previous_head() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await;

    alice
        .commit(&a, &[("g", &noise(1, 10_000)[..])], "two")
        .await;
    let calls_before = env.fault.calls().len();
    env.fault
        .add_rule(Op::PutFile, "/packs/", Action::Fail, None);
    env.fault.add_rule(Op::Put, "/packs/", Action::Fail, None);
    alice.fails(&a, &["push", "origin", "main"]).await;
    env.fault.clear_rules();

    assert_eq!(env.head().await.revision, before.revision);
    assert_eq!(env.stored_refs().await["refs/heads/main"], c1);
    let calls = env.fault.calls();
    let later = &calls[calls_before..];
    assert!(later.iter().any(|c| c.key.contains("/packs/")));
    assert!(
        !later
            .iter()
            .any(|c| c.op == Op::Put
                && (c.key.ends_with("head.json") || c.key.contains("/entries/"))),
        "no entry or head write after a failed pack upload"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_larger_than_the_receive_limit_is_rejected_without_changes() {
    let mut limits = test_limits();
    limits.max_receive_bytes = 256 * 1024;
    let env = Env::with_limits(limits);
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await;

    alice
        .commit(&a, &[("big.bin", &noise(7, 2 * 1024 * 1024)[..])], "big")
        .await;
    alice.fails(&a, &["push", "origin", "main"]).await;
    assert_eq!(env.head().await.revision, before.revision);
    assert_eq!(env.stored_refs().await["refs/heads/main"], c1);
    // Nothing was uploaded for the rejected push.
    assert_eq!(env.push_entries().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_without_disk_space_is_rejected_before_receipt() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    alice.commit(&a, &[("f", b"2")], "two").await;
    *server.running.state.disk.free_override.lock().unwrap() = Some(0);
    let err = alice.fails(&a, &["push", "origin", "main"]).await;
    assert!(err.contains("507"), "{err}");
    assert_eq!(env.stored_refs().await["refs/heads/main"], c1);
    assert_eq!(
        server
            .running
            .state
            .registry
            .svc
            .metrics
            .get("disk_limit_rejections"),
        1
    );
    // Reads still work.
    assert_eq!(alice.ls_remote(&server.url()).await["refs/heads/main"], c1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_to_an_empty_repository_with_no_commits_changes_nothing() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    // Nothing to push: Git sends no commands; the head stays at generation 0.
    alice.ok(&a, &["push", "--quiet", "origin", "--all"]).await;
    assert_eq!(env.head().await.generation, 0);
    assert!(alice.ls_remote(&server.url()).await.is_empty());
    let bob = env.client("bob");
    let b = bob.clone(&server.url_as("bob"), "empty").await;
    assert!(bob.run(&b, &["rev-parse", "HEAD"]).await.status.code() != Some(0));
    assert_eq!(
        env.history().await[0].operation,
        Operation::Create {
            default_branch: rustgit::model::branch_ref("main").unwrap()
        }
    );
}
