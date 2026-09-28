//! Stage 2 (IMPLEMENTATION_PLAN.md items 5-6): requests to different
//! servers, preferred server selection, and change notifications. Routing
//! only reduces duplicate work: every request checks the storage head.

mod common;

use std::time::{Duration, Instant};

use common::*;
use rustgit::routing::rank;

/// Git smart HTTP keeps no session between requests. With a balancer that
/// sends every request to the next server, each clone, fetch, and push
/// uses several servers.
#[tokio::test(flavor = "multi_thread")]
async fn git_operations_work_when_each_request_goes_to_another_server() {
    let env = Env::new();
    let servers = [env.start().await, env.start().await, env.start().await];
    let lb = Balancer::start(&servers.iter().collect::<Vec<_>>()).await;
    let alice = env.client("alice");
    let bob = env.client("bob");
    let a = alice.init(&lb.url_as("alice"), "work").await;
    alice
        .commit(&a, &[("f", &text(300, "base")[..])], "base")
        .await;
    alice
        .ok(&a, &["push", "--quiet", "-u", "origin", "main"])
        .await;
    let b = bob.clone(&lb.url_as("bob"), "work").await;
    for i in 0..4 {
        let c = alice
            .commit(&a, &[("f", &text(300 + i, "change")[..])], &format!("c{i}"))
            .await;
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
        bob.ok(&b, &["pull", "--quiet", "--ff-only"]).await;
        assert_eq!(bob.rev(&b, "HEAD").await, c);
        assert_eq!(
            tree_files(&bob, &b, "HEAD").await,
            tree_files(&alice, &a, "HEAD").await
        );
    }
    let tagged = bob.commit(&b, &[("g", b"bob")], "bob").await;
    bob.ok(&b, &["tag", "-a", "v1", "-m", "v1"]).await;
    bob.ok(&b, &["push", "--quiet", "origin", "main", "v1"])
        .await;
    alice
        .ok(&a, &["pull", "--quiet", "--ff-only", "--tags"])
        .await;
    assert_eq!(alice.rev(&a, "v1^{commit}").await, tagged);

    assert_eq!(lb.servers_used().len(), 3);
    // Git's first request of an operation has no credentials and gets an
    // authentication challenge, so count the servers that did Git work.
    let working = servers
        .iter()
        .filter(|s| s.metric("cache_rebuilds") >= 1)
        .count();
    assert!(working >= 2, "{working} servers did Git work");
    for s in &servers {
        assert_eq!(alice.ls_remote(&s.url()).await, env.stored_refs().await);
    }
}

/// Requests go to the preferred server of the repository. When it stops,
/// the next available server takes over.
#[tokio::test(flavor = "multi_thread")]
async fn requests_go_to_the_preferred_server_and_move_when_it_stops() {
    let env = Env::new();
    let mut servers = env.cluster(3, |cfg| cfg.routing.forward = true).await;
    let order = rank(env.repo, ["s0", "s1", "s2"]);
    let index = |id: &str| id[1..].parse::<usize>().unwrap();
    let (preferred, second, entry) = (index(order[0]), index(order[1]), index(order[2]));

    // The client always talks to the least preferred server.
    let alice = env.client("alice");
    let url = servers[entry].url();
    let a = alice.init(&url, "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let bob = env.client("bob");
    let b = bob.clone(&servers[entry].url_as("bob"), "copy").await;
    assert_eq!(bob.rev(&b, "HEAD").await, c1);

    assert!(servers[entry].metric("forwarded_requests") >= 3);
    assert_eq!(servers[preferred].metric("pushes_accepted"), 1);
    assert_eq!(servers[entry].metric("pushes"), 0);
    assert_eq!(
        servers[entry].metric("cache_rebuilds"),
        0,
        "only the preferred server built a cache"
    );

    // The preferred server stops; the next one in the ranking takes over.
    let stopped = servers.remove(preferred);
    stopped.running.stop();
    let (second, entry) = (
        second - (second > preferred) as usize,
        entry - (entry > preferred) as usize,
    );
    assert_eq!(alice.ls_remote(&url).await["refs/heads/main"], c1);
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    let mut pushed = false;
    // The first write after the stop can report "retry"; the client retries.
    for _ in 0..3 {
        if alice
            .run(&a, &["push", "--quiet", "origin", "main"])
            .await
            .status
            .success()
        {
            pushed = true;
            break;
        }
    }
    assert!(pushed);
    assert_eq!(env.stored_refs().await["refs/heads/main"], c2);
    let handled = if second == entry { entry } else { second };
    assert_eq!(servers[handled].metric("pushes_accepted"), 1);
}

/// An accepted push notifies the other servers, which update their caches
/// before a client asks. Notifications need the peer token.
#[tokio::test(flavor = "multi_thread")]
async fn accepted_pushes_notify_other_servers() {
    let env = Env::new();
    let servers = env.cluster(2, |cfg| cfg.routing.notify = true).await;
    let alice = env.client("alice");
    let a = alice.init(&servers[0].url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    alice.ls_remote(&servers[1].url()).await;
    let other = servers[1].running.registry().controller(env.repo);
    let generation = other.cached_generation().await.unwrap();
    let received = servers[1].metric("notifications_received");

    alice.commit(&a, &[("f", b"2")], "two").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let start = Instant::now();
    while other.cached_generation().await != Some(generation + 1) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the notified server updates its cache"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(servers[1].metric("notifications_received"), received + 1);

    let http = reqwest::Client::new();
    let body = format!(r#"{{"repo":"{}","generation":1}}"#, env.repo);
    let url = format!("{}/_rustgit/notify", servers[1].base());
    let r = http.post(&url).body(body.clone()).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let r = http
        .post(&url)
        .header("x-rustgit-peer-token", "peer-secret")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
}
