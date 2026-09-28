//! Health checks for load balancers, and clean stops: a stopping server
//! first fails its health check, then finishes running requests and pushes.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use rustgit::routing::rank;
use rustgit::storage::fault::{Action, Op};

async fn get(url: &str) -> (u16, String, bool) {
    let resp = reqwest::get(url).await.expect("server answers");
    let draining = resp.headers().contains_key("x-rustgit-draining");
    (resp.status().as_u16(), resp.text().await.unwrap(), draining)
}

async fn accepts_connections(addr: std::net::SocketAddr) -> bool {
    tokio::net::TcpStream::connect(addr).await.is_ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_health_check_fails_while_storage_does_not_answer() {
    let env = Env::new();
    let server = env.start().await;
    let health = format!("{}/_rustgit/health", server.base());
    let live = format!("{}/_rustgit/live", server.base());
    assert_eq!(get(&health).await.0, 200);

    env.fault.set_unavailable(true);
    // Answers are reused for one second.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (status, body, _) = get(&health).await;
    assert_eq!((status, body.as_str()), (503, "storage unavailable\n"));
    assert_eq!(get(&live).await.0, 200, "the process still runs");
    assert!(server.metric("health_storage_failures") >= 1);

    env.fault.set_unavailable(false);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(get(&health).await.0, 200);
}

/// During the drain delay the load balancer sees a failed health check,
/// while requests that still arrive are served.
#[tokio::test(flavor = "multi_thread")]
async fn a_stopping_server_fails_its_health_check_and_still_serves_requests() {
    let env = Env::new();
    let server = env
        .start_with(|cfg| cfg.server.drain_delay_secs = 2, "s0")
        .await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let (url, base, addr) = (server.url(), server.base(), server.running.addr);
    assert!(!get(&format!("{base}/_rustgit/live")).await.2);

    let started = Instant::now();
    let stopping = tokio::spawn(server.running.shutdown());
    let (status, body, draining) = get(&format!("{base}/_rustgit/health")).await;
    assert_eq!((status, body.as_str()), (503, "draining\n"));
    assert!(draining, "responses tell other servers that this one stops");
    assert_eq!(alice.ls_remote(&url).await, env.stored_refs().await);
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], c2);

    tokio::time::timeout(Duration::from_secs(30), stopping)
        .await
        .unwrap()
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert!(!accepts_connections(addr).await);
}

/// A push whose procedure is still running when the stop begins is
/// completed and answered; new connections are refused meanwhile.
#[tokio::test(flavor = "multi_thread")]
async fn a_running_push_completes_before_the_server_stops() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await.generation;
    let addr = server.running.addr;

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

    let stopping = tokio::spawn(server.running.shutdown());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !accepts_connections(addr).await,
        "new connections are refused"
    );
    assert!(!stopping.is_finished(), "the server waits for the push");
    assert!(push.try_wait().unwrap().is_none());

    release.notify_one();
    let status = tokio::time::timeout(Duration::from_secs(30), push.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "the client gets its answer");
    tokio::time::timeout(Duration::from_secs(30), stopping)
        .await
        .expect("the server stops after the push")
        .unwrap();
    assert_eq!(env.head().await.generation, before + 1);
    assert_eq!(env.stored_refs().await["refs/heads/main"], c2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_shutdown_timeout_bounds_the_stop() {
    let env = Env::new();
    let server = env
        .start_with(|cfg| cfg.server.shutdown_timeout_secs = 1, "s0")
        .await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    alice.commit(&a, &[("f", b"2")], "two").await;
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

    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(10), server.running.shutdown())
        .await
        .expect("the timeout bounds the stop");
    assert!(started.elapsed() < Duration::from_secs(5));
    let status = tokio::time::timeout(Duration::from_secs(30), push.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success(), "the push got no success answer");
    release.notify_one();
}

/// A server that forwards to a stopping server sees the draining header
/// and handles the next requests itself.
#[tokio::test(flavor = "multi_thread")]
async fn other_servers_stop_forwarding_to_a_stopping_server() {
    let env = Env::new();
    let mut servers = env
        .cluster(2, |cfg| {
            cfg.routing.forward = true;
            cfg.server.drain_delay_secs = 5;
        })
        .await;
    let order = rank(env.repo, ["s0", "s1"]);
    let preferred = order[0][1..].parse::<usize>().unwrap();
    let preferred = servers.remove(preferred);
    let other = servers.pop().unwrap();
    let alice = env.client("alice");
    let a = alice.init(&other.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let forwarded = other.metric("forwarded_requests");
    alice.ls_remote(&other.url()).await;
    assert!(
        other.metric("forwarded_requests") > forwarded,
        "forwarding works"
    );

    let stopping = tokio::spawn(preferred.running.shutdown());
    tokio::time::sleep(Duration::from_millis(100)).await;
    alice.ls_remote(&other.url()).await;
    let forwarded = other.metric("forwarded_requests");
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(alice.ls_remote(&other.url()).await["refs/heads/main"], c2);
    assert_eq!(
        other.metric("forwarded_requests"),
        forwarded,
        "requests after the draining answer stay on this server"
    );
    stopping.abort();
}
