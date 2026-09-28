//! Limits (IMPLEMENTATION_PLAN.md test group): too many processes, slow
//! clients, canceled requests, repeated retries, customer limits, and object
//! sizes. Full disks and receive limits are in stage1_local.rs.

mod common;

use std::time::{Duration, Instant};

use common::*;
use rustgit::config::{CustomerConfig, RepoConfig};
use rustgit::model::RepoId;
use rustgit::storage::fault::{Action, Op};

/// With one Git process allowed, a slow client makes other requests wait
/// and then get a busy answer. When the slow client disconnects, its
/// process is stopped and the slot is free again.
#[tokio::test(flavor = "multi_thread")]
async fn too_many_processes_get_a_busy_answer_and_a_canceled_request_frees_its_process() {
    let mut limits = test_limits();
    limits.max_git_processes = 1;
    limits.process_wait_secs = 1;
    let env = Env::with_limits(limits);
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let tip = alice
        .commit(&a, &[("big.bin", &noise(2, 16 * 1024 * 1024)[..])], "big")
        .await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let slow = start_slow_fetch(&server, "carol", &tip).await;
    let running = || server.metric("git_processes_started") - server.metric("git_processes_exited");
    assert_eq!(running(), 1, "the slow fetch holds a process");
    let err = alice
        .fails(&alice.home, &["ls-remote", &server.url()])
        .await;
    assert!(err.contains("503"), "{err}");
    assert!(server.metric("process_limit_rejections") >= 1);

    drop(slow);
    let start = Instant::now();
    while running() > 0 {
        assert!(start.elapsed() < Duration::from_secs(10), "process stopped");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(alice.ls_remote(&server.url()).await["refs/heads/main"], tip);
}

/// Every CAS conflicts: the push stops after the retry limit with a
/// retryable answer, and nothing changes.
#[tokio::test(flavor = "multi_thread")]
async fn repeated_conflicts_end_with_a_retryable_answer_at_the_retry_limit() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await;

    alice.commit(&a, &[("f", b"2")], "two").await;
    let calls = env.fault.calls().len();
    env.fault
        .add_rule(Op::Put, "head.json", Action::Conflict, None);
    let err = alice.fails(&a, &["push", "origin", "main"]).await;
    env.fault.clear_rules();
    assert!(err.contains("concurrent changes; retry the push"), "{err}");
    let attempts = env.fault.calls()[calls..]
        .iter()
        .filter(|c| c.key.ends_with("head.json") && c.outcome == "injected-conflict")
        .count();
    assert_eq!(attempts, env.limits.push_retry_limit as usize);
    assert_eq!(env.head().await.revision, before.revision);
    assert_eq!(env.stored_refs().await["refs/heads/main"], c1);
    assert_eq!(server.metric("push_conflicts_exhausted"), 1);
}

fn with_customer(cfg: &mut rustgit::config::Config, customer: CustomerConfig) {
    cfg.repos[0].customer = Some(customer.name.clone());
    cfg.customers.push(customer);
    cfg.repos.push(RepoConfig::new("other", RepoId::new()));
}

fn customer(name: &str) -> CustomerConfig {
    CustomerConfig {
        name: name.into(),
        max_git_processes: None,
        max_requests_per_minute: None,
        max_receive_bytes_per_hour: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_customer_at_its_process_limit_does_not_block_other_customers() {
    let env = Env::new();
    let server = env
        .start_with(
            |cfg| {
                with_customer(
                    cfg,
                    CustomerConfig {
                        max_git_processes: Some(1),
                        ..customer("acme")
                    },
                )
            },
            "customers",
        )
        .await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let tip = alice
        .commit(&a, &[("big.bin", &noise(3, 16 * 1024 * 1024)[..])], "big")
        .await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let slow = start_slow_fetch(&server, "carol", &tip).await;
    let start = Instant::now();
    let err = alice
        .fails(&alice.home, &["ls-remote", &server.url()])
        .await;
    assert!(err.contains("429"), "{err}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "a customer limit answers at once"
    );
    // Another customer's repository is served.
    assert!(
        alice
            .ls_remote(&server.url_for("alice", "other"))
            .await
            .is_empty()
    );
    drop(slow);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_customer_over_its_request_rate_is_told_to_retry_later() {
    let env = Env::new();
    let server = env
        .start_with(
            |cfg| {
                with_customer(
                    cfg,
                    CustomerConfig {
                        max_requests_per_minute: Some(4),
                        ..customer("acme")
                    },
                )
            },
            "rate",
        )
        .await;
    let http = reqwest::Client::new();
    let get = |repo: &str| {
        http.get(format!(
            "{}/{repo}.git/info/refs?service=git-upload-pack",
            server.base()
        ))
        .basic_auth("carol", Some(password("carol")))
        .send()
    };
    for _ in 0..4 {
        assert_eq!(get(REPO).await.unwrap().status(), 200);
    }
    let r = get(REPO).await.unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["retry-after"], "60");
    assert_eq!(get("other").await.unwrap().status(), 200);
    assert_eq!(server.metric("customer_limit_rejections"), 1);
}

/// Pushes over a customer's hourly traffic budget are rejected: with a
/// known size before receipt, and during receipt for streamed requests.
#[tokio::test(flavor = "multi_thread")]
async fn a_customer_over_its_received_bytes_budget_cannot_push() {
    let env = Env::new();
    let server = env
        .start_with(
            |cfg| {
                with_customer(
                    cfg,
                    CustomerConfig {
                        max_receive_bytes_per_hour: Some(200_000),
                        ..customer("acme")
                    },
                )
            },
            "bytes",
        )
        .await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "small").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    // Smaller than Git's post buffer: sent with a Content-Length.
    alice
        .commit(&a, &[("g", &noise(4, 300_000)[..])], "known size")
        .await;
    let err = alice.fails(&a, &["push", "origin", "main"]).await;
    assert!(err.contains("429"), "{err}");
    // Larger than the post buffer: streamed, stopped during receipt.
    alice
        .commit(&a, &[("h", &noise(5, 3_000_000)[..])], "streamed")
        .await;
    alice.fails(&a, &["push", "origin", "main"]).await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], c1);
    assert_eq!(env.push_entries().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn objects_over_the_size_limits_reject_the_push() {
    let mut limits = test_limits();
    limits.max_object_bytes = 100_000;
    limits.max_expanded_bytes = 300_000;
    let env = Env::with_limits(limits);
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice
        .commit(&a, &[("ok.bin", &noise(1, 90_000)[..])], "fits")
        .await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    alice
        .commit(&a, &[("large.bin", &noise(2, 150_000)[..])], "large object")
        .await;
    let err = alice.fails(&a, &["push", "origin", "main"]).await;
    assert!(err.contains("object larger than 100000 bytes"), "{err}");

    // Each object fits, but the new objects together do not. Objects that
    // the server already has do not count.
    alice.ok(&a, &["reset", "--quiet", "--hard", &c1]).await;
    let files: Vec<(String, Vec<u8>)> = (0..4)
        .map(|i| (format!("part{i}.bin"), noise(10 + i, 90_000)))
        .collect();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), &d[..])).collect();
    alice.commit(&a, &refs, "many parts").await;
    let err = alice
        .fails(&a, &["push", "--force", "origin", "main"])
        .await;
    assert!(
        err.contains("push expands to more than 300000 bytes"),
        "{err}"
    );

    assert_eq!(env.stored_refs().await["refs/heads/main"], c1);
    assert_eq!(env.push_entries().await.len(), 1);
    assert_eq!(server.metric("object_limit_rejections"), 2);
}
