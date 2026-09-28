//! Reads (IMPLEMENTATION_PLAN.md test group): long fetches, overlapping
//! pushes, cache replacement, partial local changes, and cache removal.

mod common;

use std::time::Duration;

use common::*;

/// Extracts the pack from a version 0 upload-pack answer without side band.
fn pack_of(response: &[u8]) -> &[u8] {
    let at = response
        .windows(4)
        .position(|w| w == b"PACK")
        .expect("pack in the answer");
    &response[at..]
}

/// A slow client holds a fetch open. A push to the same repository still
/// completes, and the fetch finishes with the state it started with.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_fetch_does_not_block_a_push_and_keeps_its_state() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice
        .commit(&a, &[("big.bin", &noise(1, 16 * 1024 * 1024)[..])], "big")
        .await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let slow = start_slow_fetch(&server, "carol", &c1).await;
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    tokio::time::timeout(
        Duration::from_secs(30),
        alice.ok(&a, &["push", "--quiet", "origin", "main"]),
    )
    .await
    .expect("the push does not wait for the fetch");
    assert_eq!(alice.ls_remote(&server.url()).await["refs/heads/main"], c2);

    // The slow fetch completes with the complete earlier state.
    let data = slow.bytes().await.unwrap();
    let pack = pack_of(&data);
    let carol = env.client("carol");
    let dir = carol.home.join("unpacked");
    std::fs::create_dir_all(&dir).unwrap();
    carol.ok(&dir, &["init", "--quiet", "--bare"]).await;
    std::fs::write(dir.join("objects/pack/fetched.pack"), pack).unwrap();
    carol
        .ok(&dir, &["index-pack", "objects/pack/fetched.pack"])
        .await;
    let size = carol
        .ok(&dir, &["cat-file", "-s", &format!("{c1}:big.bin")])
        .await;
    assert_eq!(size, (16 * 1024 * 1024).to_string());
    assert!(
        carol
            .run(&dir, &["cat-file", "-e", &c2])
            .await
            .status
            .code()
            != Some(0),
        "the fetch uses the state it started with"
    );
}

/// Local ref changes that storage does not hold are never served after a
/// restart: the leftover cache is rebuilt.
#[tokio::test(flavor = "multi_thread")]
async fn a_partial_local_change_is_not_served_after_a_restart() {
    let env = Env::new();
    let server = env.start_with(|_| {}, "partial").await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let stored = env.stored_refs().await;
    let cache_dir = server.cache_dir.clone();
    server.running.stop();

    let repo_dir = std::fs::read_dir(cache_dir.join("repos").join(env.repo.to_string()))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "git"))
        .unwrap();
    let git_dir = format!("--git-dir={}", repo_dir.display());
    alice
        .ok(&a, &[&git_dir, "update-ref", "refs/heads/main", &c1])
        .await;
    alice
        .ok(&a, &[&git_dir, "update-ref", "refs/heads/local", &c2])
        .await;

    let restarted = env.start_with(|_| {}, "partial").await;
    assert_eq!(alice.ls_remote(&restarted.url()).await, stored);
    assert_eq!(restarted.metric("cache_rebuilds"), 1);
    let cache_dir = restarted.cache_dir.clone();
    restarted.running.stop();

    // A lock file left by a stopped ref transaction also forces a rebuild;
    // otherwise it would block every later push.
    let repo_dir = std::fs::read_dir(cache_dir.join("repos").join(env.repo.to_string()))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "git"))
        .unwrap();
    std::fs::write(repo_dir.join("refs/heads/main.lock"), format!("{c1}\n")).unwrap();
    let again = env.start_with(|_| {}, "partial").await;
    assert_eq!(alice.ls_remote(&again.url()).await, stored);
    assert_eq!(again.metric("cache_rebuilds"), 1);
    let c3 = alice.commit(&a, &[("f", b"3")], "three").await;
    alice
        .ok(&a, &["push", "--quiet", &again.url(), "main"])
        .await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], c3);
}

/// A cache replacement while a reader uses the old cache: the reader keeps
/// a complete view; the old files go away after it finishes.
#[tokio::test(flavor = "multi_thread")]
async fn a_cache_replacement_lets_active_readers_finish() {
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

    // Replace the cache through the admin interface.
    let r = reqwest::Client::new()
        .post(format!("{}/_rustgit/rebuild?repo={REPO}", server.base()))
        .basic_auth("admin", Some(password("admin")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(server.metric("cache_rebuilds"), 2);
    let caches: Vec<_> =
        std::fs::read_dir(server.cache_dir.join("repos").join(env.repo.to_string()))
            .unwrap()
            .collect();
    assert_eq!(caches.len(), 2, "the old cache remains while it is in use");

    let git_dir = |d: &std::path::Path| format!("--git-dir={}", d.display());
    assert_eq!(
        alice
            .ok(&a, &[&git_dir(&old.dir), "rev-parse", "refs/heads/main"])
            .await,
        c1
    );
    alice
        .ok(
            &a,
            &[&git_dir(&old.dir), "cat-file", "-p", &format!("{c1}:f")],
        )
        .await;
    assert_eq!(alice.ls_remote(&server.url()).await["refs/heads/main"], c2);
    drop(old);
    let caches: Vec<_> =
        std::fs::read_dir(server.cache_dir.join("repos").join(env.repo.to_string()))
            .unwrap()
            .collect();
    assert_eq!(
        caches.len(),
        1,
        "the old cache is removed after its last reader"
    );
}

/// Simultaneous first reads on a server with an empty cache start one
/// reconstruction, not one per request.
#[tokio::test(flavor = "multi_thread")]
async fn simultaneous_reads_on_an_empty_cache_rebuild_it_once() {
    let env = Env::new();
    let first = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&first.url(), "work").await;
    let tip = alice.commit(&a, &[("f", &text(500, "x")[..])], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let fresh = env.start().await;
    let clients: Vec<Client> = (0..6).map(|i| env.client(&format!("r{i}"))).collect();
    let url = fresh.url_as("carol");
    let heads = futures::future::join_all(clients.iter().map(|c| c.ls_remote(&url))).await;
    for h in heads {
        assert_eq!(h["refs/heads/main"], tip);
    }
    assert_eq!(fresh.metric("cache_rebuilds"), 1);
}

/// Idle caches are removed, but never while a reader uses them.
#[tokio::test(flavor = "multi_thread")]
async fn idle_caches_are_removed_only_when_no_operation_uses_them() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let tip = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let registry = server.running.registry();
    let dir = server.cache_dir.join("repos").join(env.repo.to_string());

    let view = registry.controller(env.repo).prepare_read().await.unwrap();
    assert_eq!(registry.evict_idle(Duration::ZERO).await, 0);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    drop(view);
    assert_eq!(registry.evict_idle(Duration::ZERO).await, 1);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

    let rebuilds = server.metric("cache_rebuilds");
    assert_eq!(alice.ls_remote(&server.url()).await["refs/heads/main"], tip);
    assert_eq!(server.metric("cache_rebuilds"), rebuilds + 1);
}
