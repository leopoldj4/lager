//! Stage 3 checks (IMPLEMENTATION_PLAN.md): snapshots, data removal,
//! backup, repository lifetime, and customer separation.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use rustgit::config::RepoConfig;
use rustgit::maintenance::{CleanupOptions, cleanup};
use rustgit::model::{Operation, RepoId, RepoState, branch_ref};
use rustgit::repository::admin;
use rustgit::storage::fault::{Action, Op};
use rustgit::storage::{ObjectStoreBackend, Storage, WriteMode};

async fn push_commits(client: &Client, dir: &std::path::Path, n: usize, tag: &str) -> String {
    let mut last = String::new();
    for i in 0..n {
        last = client
            .commit(
                dir,
                &[(
                    &format!("{tag}-{i}.txt"),
                    &text(20, &format!("{tag} {i}"))[..],
                )],
                &format!("{tag} {i}"),
            )
            .await;
        client.ok(dir, &["push", "--quiet", "origin", "HEAD"]).await;
    }
    last
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_restores_the_same_state_as_the_complete_log() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice
        .ok(&a, &["push", "--quiet", "-u", "origin", "main"])
        .await;
    push_commits(&alice, &a, 3, "before").await;
    alice.ok(&a, &["tag", "-a", "v1", "-m", "v1"]).await;
    alice.ok(&a, &["push", "--quiet", "origin", "v1"]).await;
    let log_state = env.stored_refs().await;

    // Only admins may start maintenance through HTTP.
    let http = reqwest::Client::new();
    let url = format!("{}/_rustgit/snapshot?repo={REPO}", server.base());
    let r = http
        .post(&url)
        .basic_auth("alice", Some(password("alice")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = http
        .post(&url)
        .basic_auth("admin", Some(password("admin")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let report: serde_json::Value = serde_json::from_slice(&r.bytes().await.unwrap()).unwrap();

    let head = env.head().await;
    let pointer = head.snapshot.clone().expect("snapshot attached");
    assert_eq!(report["base_generation"], pointer.base_generation);
    let snap = env
        .storage
        .get_snapshot(env.repo, pointer.id)
        .await
        .unwrap();
    let snap_refs: BTreeMap<String, String> = snap
        .refs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(snap_refs, log_state);
    assert_eq!(env.stored_refs().await, log_state);

    // A push after the snapshot, then recovery from snapshot plus entries.
    let tip = push_commits(&alice, &a, 1, "after").await;
    let expected = env.stored_refs().await;
    assert_eq!(expected["refs/heads/main"], tip);
    check_history(&env, &[]).await;
    server.stop_and_remove_cache();
    let fresh = env.start().await;
    let bob = env.client("bob");
    let b = bob.clone(&fresh.url_as("bob"), "copy").await;
    assert_eq!(bob.clone_refs(&b).await, expected);
    assert_eq!(
        tree_files(&bob, &b, "HEAD").await,
        tree_files(&alice, &a, "HEAD").await
    );
    bob.ok(&b, &["fsck", "--full", "--strict"]).await;
}

/// The snapshot's CAS loses against a push on another server. The snapshot
/// is attached after the push, and the push is retained.
#[tokio::test(flavor = "multi_thread")]
async fn a_push_that_overlaps_a_snapshot_is_retained() {
    let env = Env::new();
    let maint = env.start().await;
    let other = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&other.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    push_commits(&alice, &a, 2, "x").await;

    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    // The first entry upload after this rule is the snapshot's entry.
    env.fault.add_rule(
        Op::Put,
        "/entries/",
        Action::Gate(reached.clone(), release.clone()),
        Some(1),
    );
    let controller = maint.running.registry().controller(env.repo);
    let snap =
        tokio::spawn(async move { rustgit::maintenance::snapshot(&controller, "test").await });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .unwrap();
    let pushed = push_commits(&alice, &a, 1, "overlap").await;
    release.notify_one();
    let report = snap.await.unwrap().unwrap();

    assert_eq!(report.attempts, 2, "the first snapshot CAS conflicted");
    let head = env.head().await;
    assert_eq!(head.generation, report.generation);
    assert!(head.snapshot.as_ref().unwrap().base_generation < head.generation - 1);
    assert_eq!(env.stored_refs().await["refs/heads/main"], pushed);
    check_history(&env, &[(pushed.clone(), PushResult::Accepted)]).await;
    // Both servers and a new one serve the push.
    let fresh = env.start().await;
    for s in [&maint, &other, &fresh] {
        assert_eq!(alice.ls_remote(&s.url()).await["refs/heads/main"], pushed);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn old_read_views_remain_usable_until_their_readers_finish() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let controller = server.running.registry().controller(env.repo);
    let old = controller.prepare_read().await.unwrap();
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let new = controller.prepare_read().await.unwrap();
    assert!(new.generation > old.generation);

    let git_dir = |dir: &std::path::Path| format!("--git-dir={}", dir.display());
    assert_eq!(
        alice
            .ok(&a, &[&git_dir(&old.dir), "rev-parse", "refs/heads/main"])
            .await,
        c1
    );
    assert_eq!(
        alice
            .ok(&a, &[&git_dir(&new.dir), "rev-parse", "refs/heads/main"])
            .await,
        c2
    );
    // The old view is complete: its objects are readable.
    alice
        .ok(
            &a,
            &[&git_dir(&old.dir), "cat-file", "-p", &format!("{c1}:f")],
        )
        .await;
    let old_dir = old.dir.clone();
    drop(old);
    assert!(
        !old_dir.exists(),
        "the view is removed after its last reader"
    );
    assert!(new.dir.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn data_removal_reports_first_and_keeps_recovery_possible() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    push_commits(&alice, &a, 3, "old").await;
    let controller = server.running.registry().controller(env.repo);
    rustgit::maintenance::snapshot(&controller, "test")
        .await
        .unwrap();
    push_commits(&alice, &a, 2, "new").await;
    let expected = env.stored_refs().await;

    let before = env.storage.list_repo(env.repo).await.unwrap().len();
    let report = cleanup(
        &env.storage,
        env.repo,
        &CleanupOptions::report(Duration::ZERO),
    )
    .await
    .unwrap();
    assert!(report.report_only);
    assert_eq!(report.removed, 0);
    // The packs of the four pushes before the snapshot are covered by it.
    let packs: Vec<_> = report
        .candidates
        .iter()
        .filter(|c| c.key.contains("/packs/"))
        .collect();
    assert_eq!(packs.len(), 4, "{:#?}", report.candidates);
    assert_eq!(env.storage.list_repo(env.repo).await.unwrap().len(), before);

    let removed = cleanup(
        &env.storage,
        env.repo,
        &CleanupOptions::remove(Duration::ZERO),
    )
    .await
    .unwrap();
    assert_eq!(removed.removed, report.candidates.len());
    assert!(removed.fence_generation.is_some());
    // Everything reported is gone; the fence entry is new.
    assert_eq!(
        env.storage.list_repo(env.repo).await.unwrap().len(),
        before - removed.removed + 1
    );

    // Recovery works from what remains, and the running server still works.
    let fresh = env.start().await;
    let bob = env.client("bob");
    let b = bob.clone(&fresh.url_as("bob"), "copy").await;
    assert_eq!(bob.clone_refs(&b).await, expected);
    bob.ok(&b, &["fsck", "--full", "--strict"]).await;
    let tip = push_commits(&alice, &a, 1, "later").await;
    assert_eq!(
        bob.ls_remote(&fresh.url_as("bob")).await["refs/heads/main"],
        tip
    );
}

/// Data removal during a push that has uploaded its pack and entry but has
/// not attached them yet. The retention period protects those uploads.
#[tokio::test(flavor = "multi_thread")]
async fn data_removal_does_not_break_an_active_push() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    env.fault.add_rule(
        Op::Put,
        "head.json",
        Action::Gate(reached.clone(), release.clone()),
        Some(1),
    );
    let c2 = alice
        .commit(&a, &[("g", &noise(3, 5000)[..])], "active")
        .await;
    let push = tokio::spawn({
        let (home, dir) = (alice.home.clone(), a.clone());
        async move { push_result(&Client { home }, &dir, &["push", "origin", "main"]).await }
    });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .unwrap();
    let report = cleanup(
        &env.storage,
        env.repo,
        &CleanupOptions::remove(Duration::from_secs(3600)),
    )
    .await
    .unwrap();
    assert_eq!(report.removed, 0);
    assert!(
        report.retained_recent >= 2,
        "pack and entry of the active push"
    );
    release.notify_one();
    assert_eq!(push.await.unwrap(), PushResult::Accepted);

    server.stop_and_remove_cache();
    let fresh = env.start().await;
    let bob = env.client("bob");
    let b = bob.clone(&fresh.url_as("bob"), "copy").await;
    assert_eq!(bob.rev(&b, "HEAD").await, c2);
    assert_eq!(std::fs::read(b.join("g")).unwrap(), noise(3, 5000));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backup_restores_into_an_empty_independent_system() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    push_commits(&alice, &a, 2, "a").await;
    let controller = server.running.registry().controller(env.repo);
    rustgit::maintenance::snapshot(&controller, "test")
        .await
        .unwrap();
    alice.ok(&a, &["checkout", "--quiet", "-b", "side"]).await;
    push_commits(&alice, &a, 2, "b").await;
    alice.ok(&a, &["tag", "-a", "v1", "-m", "v1"]).await;
    alice.ok(&a, &["push", "--quiet", "origin", "v1"]).await;
    let expected = env.stored_refs().await;
    let source_head = env.head().await;

    let out = env.path("backup");
    let manifest = rustgit::backup::export(&env.storage, env.repo, &out)
        .await
        .unwrap();
    assert_eq!(manifest.generation, source_head.generation);

    // A different storage service and a different server.
    let target = Storage::new(Arc::new(ObjectStoreBackend::in_memory()), "restored");
    let head = rustgit::backup::import(&target, &out).await.unwrap();
    assert_eq!(head.repository, env.repo);
    assert_eq!(head.generation, source_head.generation);
    assert_eq!(stored_refs(&target, env.repo).await, expected);
    // Importing again must not replace the restored head.
    assert!(rustgit::backup::import(&target, &out).await.is_err());

    let restored = env.start_on(target.clone(), |_| {}, "restored").await;
    let bob = env.client("bob");
    let b = bob.clone(&restored.url_as("bob"), "copy").await;
    assert_eq!(bob.clone_refs(&b).await, expected);
    assert_eq!(
        tree_files(&bob, &b, "origin/side").await,
        tree_files(&alice, &a, "side").await
    );
    bob.ok(&b, &["fsck", "--full", "--strict"]).await;
    // The restored system accepts new pushes.
    bob.commit(&b, &[("new", b"n")], "n").await;
    bob.ok(&b, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(
        load_head(&target, env.repo).await.generation,
        source_head.generation + 1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_repository_is_not_served_and_not_restored() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(alice.ls_remote(&server.url()).await.len(), 1);

    admin::delete_repository(&env.storage, env.repo, "test")
        .await
        .unwrap();
    let err = alice
        .fails(&alice.home, &["ls-remote", &server.url()])
        .await;
    assert!(err.contains("not found"), "{err}");
    alice.commit(&a, &[("f", b"1")], "1").await;
    alice.fails(&a, &["push", "origin", "main"]).await;

    // A server start does not create the configured repository again.
    let restarted = env.start().await;
    alice
        .fails(&alice.home, &["ls-remote", &restarted.url()])
        .await;
    assert_eq!(env.head().await.state, RepoState::Deleted);
    let last = env.history().await.pop().unwrap();
    assert_eq!(last.operation, Operation::Delete);
    assert!(
        admin::create_repository(&env.storage, env.repo, &branch_ref("main").unwrap(), "t")
            .await
            .is_err()
    );
}

/// A push waits for its CAS while the repository is deleted. The push must
/// not restore the repository.
#[tokio::test(flavor = "multi_thread")]
async fn a_push_that_overlaps_deletion_does_not_restore_the_repository() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    env.fault.add_rule(
        Op::Put,
        "head.json",
        Action::Gate(reached.clone(), release.clone()),
        Some(1),
    );
    let c = alice.commit(&a, &[("f", b"1")], "1").await;
    let push = tokio::spawn({
        let (home, dir) = (alice.home.clone(), a.clone());
        async move { push_result(&Client { home }, &dir, &["push", "origin", "main"]).await }
    });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .unwrap();
    admin::delete_repository(&env.storage, env.repo, "test")
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(push.await.unwrap(), PushResult::Rejected);
    assert_eq!(env.head().await.state, RepoState::Deleted);
    assert!(
        !env.history().await.iter().flat_map(|e| &e.refs).any(|u| u
            .new
            .as_ref()
            .map(|o| o.as_str())
            == Some(c.as_str()))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reused_name_starts_an_empty_repository() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let old = alice.commit(&a, &[("f", b"old")], "old").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    admin::delete_repository(&env.storage, env.repo, "test")
        .await
        .unwrap();

    // The name now maps to a new repository UUID.
    let new_id = RepoId::new();
    let renamed = env
        .start_with(|cfg| cfg.repos[0].id = new_id, "reused")
        .await;
    assert!(alice.ls_remote(&renamed.url()).await.is_empty());
    // A client without the old objects cannot fetch them by ID.
    let bob = env.client("bob");
    let b = bob.init(&renamed.url_as("bob"), "empty").await;
    let out = bob.run(&b, &["fetch", "origin", &old]).await;
    assert!(!out.status.success());
    let fresh = alice.commit(&a, &[("f", b"new")], "new").await;
    alice
        .ok(&a, &["push", "--quiet", &renamed.url(), "main"])
        .await;
    assert_eq!(
        stored_refs(&env.storage, new_id).await["refs/heads/main"],
        fresh
    );
    assert_eq!(env.head().await.state, RepoState::Deleted);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_default_branch_change_applies_to_new_clones() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"main")], "main").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    alice
        .ok(&a, &["checkout", "--quiet", "-b", "develop"])
        .await;
    let dev = alice.commit(&a, &[("f", b"develop")], "develop").await;
    alice
        .ok(&a, &["push", "--quiet", "origin", "develop"])
        .await;

    let bob = env.client("bob");
    let b = bob.clone(&server.url_as("bob"), "one").await;
    assert_eq!(
        bob.ok(&b, &["symbolic-ref", "HEAD"]).await,
        "refs/heads/main"
    );

    admin::set_default_branch(&env.storage, env.repo, branch_ref("develop").unwrap(), "t")
        .await
        .unwrap();
    let symref = bob
        .ok(
            &bob.home,
            &["ls-remote", "--symref", &server.url_as("bob"), "HEAD"],
        )
        .await;
    assert!(
        symref.starts_with("ref: refs/heads/develop\tHEAD"),
        "{symref}"
    );
    let b2 = bob.clone(&server.url_as("bob"), "two").await;
    assert_eq!(
        bob.ok(&b2, &["symbolic-ref", "HEAD"]).await,
        "refs/heads/develop"
    );
    assert_eq!(bob.rev(&b2, "HEAD").await, dev);
    // The same after complete cache loss.
    server.stop_and_remove_cache();
    let fresh = env.start().await;
    let b3 = bob.clone(&fresh.url_as("bob"), "three").await;
    assert_eq!(bob.rev(&b3, "HEAD").await, dev);
}

/// A damaged pack in storage is detected. The server does not serve a
/// partial repository.
#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_pack_is_detected_and_not_served() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice
        .commit(&a, &[("f", &noise(9, 20_000)[..])], "one")
        .await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let pack = env.push_entries().await[0].packs[0].clone();
    let key = env.storage.pack_key(env.repo, &pack.sha256);
    let backend = env.storage.backend();
    let etag = backend.get(&key, None).await.unwrap().etag.unwrap();
    backend
        .put(
            &key,
            noise(10, pack.size as usize).into(),
            WriteMode::Update(etag),
        )
        .await
        .unwrap();

    server.stop_and_remove_cache();
    let fresh = env.start().await;
    let bob = env.client("bob");
    let err = bob
        .fails(&bob.home, &["clone", &fresh.url_as("bob"), "copy"])
        .await;
    assert!(err.contains("500"), "{err}");
    assert!(!bob.home.join("copy").exists());
}

/// Two customers: each user sees only the repositories granted to them,
/// even with known object IDs.
#[tokio::test(flavor = "multi_thread")]
async fn one_customer_cannot_read_another_customers_repository() {
    let env = Env::new();
    let other_id = RepoId::new();
    let server = env
        .start_with(
            |cfg| {
                cfg.repos.push(RepoConfig::new("other", other_id));
                // bob and carol belong to the first customer only.
                for u in &mut cfg.users {
                    if u.name == "bob" || u.name == "carol" {
                        let level = u.access.remove("*").unwrap();
                        u.access.insert(REPO.into(), level);
                    }
                }
            },
            "two",
        )
        .await;
    let other_url = |user: &str| {
        format!(
            "http://{user}:{}@{}/other.git",
            password(user),
            server.running.addr
        )
    };
    let alice = env.client("alice");
    let o = alice.init(&other_url("alice"), "other").await;
    let secret = alice
        .commit(&o, &[("secret", b"other customer")], "secret")
        .await;
    alice.ok(&o, &["push", "--quiet", "origin", "main"]).await;

    let bob = env.client("bob");
    let err = bob
        .fails(&bob.home, &["ls-remote", &other_url("bob")])
        .await;
    assert!(err.contains("not found"), "{err}");
    // Bob's own repository does not contain the other customer's objects.
    let b = bob.init(&server.url_as("bob"), "mine").await;
    bob.commit(&b, &[("f", b"1")], "1").await;
    bob.ok(&b, &["push", "--quiet", "origin", "main"]).await;
    let out = bob.run(&b, &["fetch", "origin", &secret]).await;
    assert!(!out.status.success());
    assert_eq!(
        stored_refs(&env.storage, other_id).await["refs/heads/main"],
        secret
    );
}
