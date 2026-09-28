//! Stage 2 checks (IMPLEMENTATION_PLAN.md): several servers with separate
//! caches and shared storage.

mod common;

use std::time::Duration;

use common::*;
use rustgit::storage::fault::{Action, Op};

#[tokio::test(flavor = "multi_thread")]
async fn a_read_on_server_b_includes_a_push_that_server_a_accepted() {
    let env = Env::new();
    let a_srv = env.start().await;
    let b_srv = env.start().await;
    let alice = env.client("alice");
    let bob = env.client("bob");
    let a = alice.init(&a_srv.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "zero").await;
    alice
        .ok(&a, &["push", "--quiet", "-u", "origin", "main"])
        .await;
    // Server B has a current cache before each push on A.
    let b = bob.clone(&b_srv.url_as("bob"), "work").await;
    for i in 1..=5 {
        let c = alice
            .commit(&a, &[("f", format!("{i}").as_bytes())], &format!("c{i}"))
            .await;
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
        // No notification between the servers: B must check storage.
        assert_eq!(
            bob.ls_remote(&b_srv.url_as("bob")).await["refs/heads/main"],
            c
        );
        bob.ok(&b, &["pull", "--quiet", "--ff-only"]).await;
        assert_eq!(bob.rev(&b, "HEAD").await, c);
        assert_eq!(
            std::fs::read(b.join("f")).unwrap(),
            format!("{i}").as_bytes()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn competing_pushes_from_the_same_old_value_cannot_both_succeed() {
    let env = Env::new();
    let servers = [env.start().await, env.start().await];
    let alice = env.client("alice");
    let bob = env.client("bob");
    let a = alice.init(&servers[0].url(), "work").await;
    alice.commit(&a, &[("f", b"base")], "base").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let b = bob.clone(&servers[1].url_as("bob"), "work").await;

    let mut results = Vec::new();
    for round in 0..4 {
        // Both clients start from the same stored value of main.
        let base = env.stored_refs().await["refs/heads/main"].clone();
        for (c, d) in [(&alice, &a), (&bob, &b)] {
            c.ok(d, &["fetch", "--quiet", "origin"]).await;
            c.ok(d, &["reset", "--quiet", "--hard", &base]).await;
        }
        let ca = alice
            .commit(&a, &[("f", format!("alice {round}").as_bytes())], "a")
            .await;
        let cb = bob
            .commit(&b, &[("f", format!("bob {round}").as_bytes())], "b")
            .await;
        let (ra, rb) = tokio::join!(
            push_result(&alice, &a, &["push", "origin", "main"]),
            push_result(&bob, &b, &["push", "origin", "main"]),
        );
        assert_ne!(ra, PushResult::Unknown);
        assert_ne!(rb, PushResult::Unknown);
        assert!(
            (ra == PushResult::Accepted) ^ (rb == PushResult::Accepted),
            "round {round}: exactly one push succeeds ({ra:?}, {rb:?})"
        );
        let winner = if ra == PushResult::Accepted { &ca } else { &cb };
        assert_eq!(&env.stored_refs().await["refs/heads/main"], winner);
        for s in &servers {
            assert_eq!(&alice.ls_remote(&s.url()).await["refs/heads/main"], winner);
        }
        results.push((ca, ra));
        results.push((cb, rb));
    }
    check_history(&env, &results).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_pushes_to_different_refs_all_succeed() {
    let env = Env::new();
    let servers = [env.start().await, env.start().await, env.start().await];
    let setup = env.client("setup");
    let s = setup.init(&servers[0].url(), "work").await;
    setup.commit(&s, &[("f", b"base")], "base").await;
    setup.ok(&s, &["push", "--quiet", "origin", "main"]).await;

    let clients: Vec<Client> = (0..3).map(|i| env.client(&format!("c{i}"))).collect();
    let mut dirs = Vec::new();
    for (i, c) in clients.iter().enumerate() {
        let d = c.clone(&servers[i].url(), "work").await;
        c.ok(&d, &["checkout", "--quiet", "-b", &format!("b{i}")])
            .await;
        dirs.push(d);
    }
    let mut results = Vec::new();
    for round in 0..3 {
        let mut commits = Vec::new();
        for (i, c) in clients.iter().enumerate() {
            commits.push(
                c.commit(
                    &dirs[i],
                    &[(&format!("f{i}"), format!("{round}").as_bytes())],
                    "x",
                )
                .await,
            );
        }
        let branches: Vec<String> = (0..3).map(|i| format!("b{i}")).collect();
        let outcomes = futures::future::join_all(clients.iter().enumerate().map(|(i, c)| {
            let (dir, branch) = (&dirs[i], &branches[i]);
            async move { push_result(c, dir, &["push", "origin", branch]).await }
        }))
        .await;
        for (i, r) in outcomes.into_iter().enumerate() {
            assert_eq!(r, PushResult::Accepted, "round {round}, client {i}");
            results.push((commits[i].clone(), r));
        }
        let stored = env.stored_refs().await;
        for (i, c) in commits.iter().enumerate() {
            assert_eq!(&stored[&format!("refs/heads/b{i}")], c);
        }
    }
    check_history(&env, &results).await;
}

/// An atomic push moves two refs together. Readers on another server see
/// both old values or both new values, never a mix.
#[tokio::test(flavor = "multi_thread")]
async fn readers_never_observe_part_of_a_transaction() {
    let env = Env::new();
    let writer = env.start().await;
    let reader = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&writer.url(), "work").await;
    alice.commit(&a, &[("f", b"0")], "0").await;
    alice
        .ok(
            &a,
            &[
                "push",
                "--quiet",
                "--atomic",
                "origin",
                "main",
                "main:refs/heads/mirror",
            ],
        )
        .await;

    let carol = env.client("carol");
    let reader_url = reader.url_as("carol");
    let writer_url = writer.url_as("carol");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observe = {
        let stop = stop.clone();
        let carol = &carol;
        async move {
            let mut n = 0;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                for url in [&reader_url, &writer_url] {
                    let refs = carol.ls_remote(url).await;
                    assert_eq!(
                        refs.get("refs/heads/main"),
                        refs.get("refs/heads/mirror"),
                        "partial transaction visible"
                    );
                    n += 1;
                }
            }
            n
        }
    };
    let write = async {
        for i in 1..=8 {
            alice
                .commit(&a, &[("f", format!("{i}").as_bytes())], "x")
                .await;
            alice
                .ok(
                    &a,
                    &[
                        "push",
                        "--quiet",
                        "--atomic",
                        "origin",
                        "main",
                        "main:refs/heads/mirror",
                    ],
                )
                .await;
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
    };
    let (n, ()) = tokio::join!(observe, write);
    assert!(n >= 4, "readers ran during the pushes ({n})");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_storage_access_rejects_new_reads_and_writes() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    // The cache is current, but it is not used without a storage check.
    assert_eq!(alice.ls_remote(&server.url()).await["refs/heads/main"], c1);

    env.fault.set_unavailable(true);
    let err = alice
        .fails(&alice.home, &["ls-remote", &server.url()])
        .await;
    assert!(err.contains("503"), "{err}");
    let bob = env.client("bob");
    bob.fails(&bob.home, &["clone", &server.url_as("bob"), "x"])
        .await;
    alice.commit(&a, &[("f", b"2")], "two").await;
    alice.fails(&a, &["push", "origin", "main"]).await;
    env.fault.set_unavailable(false);

    assert_eq!(env.stored_refs().await["refs/heads/main"], c1);
    assert_eq!(alice.ls_remote(&server.url()).await["refs/heads/main"], c1);
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
}

/// Storage accepted the head change, but the server never learned it and
/// told the client "unknown". The change is retained and visible everywhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_change_accepted_by_storage_without_an_answer_is_visible_everywhere() {
    let env = Env::new();
    let a_srv = env.start().await;
    let b_srv = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&a_srv.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await.generation;

    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    env.fault
        .add_rule(Op::Put, "head.json", Action::LoseResponse, None);
    let r = push_result(&alice, &a, &["push", "origin", "main"]).await;
    env.fault.clear_rules();
    assert_eq!(r, PushResult::Unknown);

    assert_eq!(env.head().await.generation, before + 1);
    for s in [&a_srv, &b_srv] {
        assert_eq!(alice.ls_remote(&s.url()).await["refs/heads/main"], c2);
    }
    check_history(&env, &[(c2, r)]).await;
    // Server A's cache is usable for the next push.
    let c3 = alice.commit(&a, &[("f", b"3")], "three").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], c3);
}

/// A slow CAS on one server while another server pushes the same ref: the
/// slow push must not overwrite the other one.
#[tokio::test(flavor = "multi_thread")]
async fn a_delayed_cas_cannot_overwrite_a_newer_push() {
    let env = Env::new();
    let a_srv = env.start().await;
    let b_srv = env.start().await;
    let alice = env.client("alice");
    let bob = env.client("bob");
    let a = alice.init(&a_srv.url(), "work").await;
    let base = alice.commit(&a, &[("f", b"base")], "base").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let b = bob.clone(&b_srv.url_as("bob"), "work").await;
    assert_eq!(bob.rev(&b, "HEAD").await, base);

    let ca = alice.commit(&a, &[("f", b"alice")], "alice").await;
    let cb = bob.commit(&b, &[("f", b"bob")], "bob").await;
    let reached = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    env.fault.add_rule(
        Op::Put,
        "head.json",
        Action::Gate(reached.clone(), release.clone()),
        Some(1),
    );
    let slow = tokio::spawn({
        let dir = a.clone();
        let home = alice.home.clone();
        async move {
            let alice = Client { home };
            push_result(&alice, &dir, &["push", "origin", "main"]).await
        }
    });
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .unwrap();
    // Bob's push passes the waiting CAS.
    bob.ok(&b, &["push", "--quiet", "origin", "main"]).await;
    release.notify_one();
    let ra = slow.await.unwrap();
    assert_eq!(ra, PushResult::Rejected);
    assert_eq!(env.stored_refs().await["refs/heads/main"], cb);
    check_history(&env, &[(ca, ra), (cb, PushResult::Accepted)]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn all_caches_lost_still_permits_recovery_from_storage() {
    let env = Env::new();
    let s1 = env.start().await;
    let s2 = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&s1.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    alice.ok(&a, &["remote", "add", "two", &s2.url()]).await;
    alice.ok(&a, &["checkout", "--quiet", "-b", "side"]).await;
    alice.commit(&a, &[("g", b"side")], "side").await;
    alice.ok(&a, &["tag", "-a", "t", "-m", "tag"]).await;
    alice.ok(&a, &["push", "--quiet", "two", "side", "t"]).await;
    let expected = env.stored_refs().await;
    assert_eq!(expected.len(), 3);

    s1.stop_and_remove_cache();
    s2.stop_and_remove_cache();
    let s3 = env.start().await;
    let bob = env.client("bob");
    let b = bob.clone(&s3.url_as("bob"), "copy").await;
    assert_eq!(bob.clone_refs(&b).await, expected);
    assert_eq!(
        tree_files(&bob, &b, "origin/side").await,
        tree_files(&alice, &a, "side").await
    );
    bob.ok(&b, &["fsck", "--full", "--strict"]).await;
}

/// Simple deterministic random numbers, so that a failing seed repeats.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Concurrent pushes to shared and separate branches on three servers, with
/// lost responses, delays, and failed uploads. The recorded history must obey
/// the transaction rules and agree with every definite client result;
/// unknown results count as unknown, never as failures.
#[tokio::test(flavor = "multi_thread")]
async fn a_history_with_random_storage_faults_obeys_the_transaction_rules() {
    let seed = std::env::var("RUSTGIT_TEST_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_cafe_u64);
    let mut rng = Rng(seed);
    let env = Env::new();
    let servers = [env.start().await, env.start().await, env.start().await];
    let setup = env.client("setup");
    let s = setup.init(&servers[0].url(), "work").await;
    setup.commit(&s, &[("f", b"base")], "base").await;
    setup.ok(&s, &["push", "--quiet", "origin", "main"]).await;

    let clients: Vec<Client> = (0..4).map(|i| env.client(&format!("w{i}"))).collect();
    let mut dirs = Vec::new();
    for c in &clients {
        dirs.push(c.clone(&servers[0].url(), "work").await);
    }
    let mut results = Vec::new();
    for round in 0..6 {
        env.fault.clear_rules();
        let faults = [
            (Op::Put, "head.json", Action::LoseResponse),
            (
                Op::Put,
                "head.json",
                Action::Delay(Duration::from_millis(150)),
            ),
            (Op::Put, "head.json", Action::Fail),
            (Op::Put, "/entries/", Action::Fail),
            (
                Op::PutFile,
                "/packs/",
                Action::Delay(Duration::from_millis(100)),
            ),
            (
                Op::Get,
                "head.json",
                Action::Delay(Duration::from_millis(50)),
            ),
        ];
        for _ in 0..2 {
            let (op, key, action) = faults[rng.below(faults.len() as u64) as usize].clone();
            env.fault
                .add_rule(op, key, action, Some(1 + rng.below(2) as usize));
        }
        let mut plans = Vec::new();
        for (i, c) in clients.iter().enumerate() {
            let server = &servers[rng.below(3) as usize];
            let branch = if rng.below(2) == 0 {
                "main".to_string()
            } else {
                format!("b{i}")
            };
            let url = server.url();
            c.ok(
                &dirs[i],
                &[
                    "fetch",
                    "--quiet",
                    &url,
                    "+refs/heads/*:refs/remotes/origin/*",
                ],
            )
            .await;
            let base = if c
                .run(
                    &dirs[i],
                    &["rev-parse", "--verify", "-q", &format!("origin/{branch}")],
                )
                .await
                .status
                .success()
            {
                format!("origin/{branch}")
            } else {
                "origin/main".to_string()
            };
            c.ok(&dirs[i], &["checkout", "--quiet", "-B", &branch, &base])
                .await;
            let commit = c
                .commit(
                    &dirs[i],
                    &[(&format!("f{i}"), format!("{round} {i}").as_bytes())],
                    "x",
                )
                .await;
            plans.push((i, url, branch, commit));
        }
        let outcomes = futures::future::join_all(plans.iter().map(|(i, url, branch, _)| {
            let (client, dir) = (&clients[*i], &dirs[*i]);
            async move {
                let spec = format!("HEAD:{branch}");
                push_result(client, dir, &["push", url, &spec]).await
            }
        }))
        .await;
        for ((_, _, _, commit), r) in plans.into_iter().zip(outcomes) {
            results.push((commit, r));
        }
    }
    env.fault.clear_rules();
    check_history(&env, &results).await;
    let accepted = results
        .iter()
        .filter(|(_, r)| *r == PushResult::Accepted)
        .count();
    assert!(accepted >= 6, "seed {seed}: {accepted} pushes accepted");
    let stored = env.stored_refs().await;
    for s in &servers {
        assert_eq!(setup.ls_remote(&s.url()).await, stored, "seed {seed}");
    }
}
