//! Request results (IMPLEMENTATION_PLAN.md test group): lost responses,
//! delayed requests, and reused identifiers with different contents. Lost
//! CAS responses are also in stage1_local.rs and stage2_servers.rs.

mod common;

use std::time::Duration;

use common::*;
use rustgit::model::{FORMAT_VERSION, Receipt};
use rustgit::storage::StorageError;
use rustgit::storage::fault::{Action, Op};

/// A request UUID names one request. Its receipt cannot be replaced by one
/// with different contents; repeating the identical receipt is harmless.
#[tokio::test(flavor = "multi_thread")]
async fn a_reused_request_identifier_with_different_contents_is_rejected() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let entry = env.push_entries().await.pop().unwrap();

    // The receipt records the result of the request.
    let receipt = env
        .storage
        .get_receipt(env.repo, entry.request)
        .await
        .unwrap()
        .expect("receipt");
    assert_eq!(receipt.entry, entry.id);
    assert_eq!(receipt.generation, entry.generation);
    assert_eq!(receipt.request_checksum, entry.request_checksum);

    env.storage.put_receipt(&receipt).await.unwrap();
    let other = Receipt {
        request_checksum: "0".repeat(64),
        ..receipt.clone()
    };
    assert!(matches!(
        env.storage.put_receipt(&other).await,
        Err(StorageError::Corrupt(_))
    ));
    // Entries are immutable in the same way.
    let mut changed = entry.clone();
    changed.user = "mallory".into();
    assert!(env.storage.put_entry(&changed).await.is_err());
    assert_eq!(
        env.storage
            .get_receipt(env.repo, entry.request)
            .await
            .unwrap()
            .unwrap(),
        receipt
    );
    assert_eq!(receipt.format, FORMAT_VERSION);
}

/// Internal retries after lost answers for entries and receipts repeat the
/// identical request; the push is applied once.
#[tokio::test(flavor = "multi_thread")]
async fn lost_answers_for_records_do_not_apply_a_push_twice() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    env.fault
        .add_rule(Op::Put, "/entries/", Action::LoseResponse, Some(1));
    env.fault
        .add_rule(Op::Put, "/receipts/", Action::LoseResponse, Some(1));
    env.fault
        .add_rule(Op::PutFile, "/packs/", Action::LoseResponse, Some(1));
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let pushes = env.push_entries().await;
    assert_eq!(pushes.len(), 2);
    check_history(&env, &[(c2.clone(), PushResult::Accepted)]).await;
    assert!(
        env.storage
            .get_receipt(env.repo, pushes[1].request)
            .await
            .unwrap()
            .is_some()
    );
}

/// Delayed storage requests slow a push down but do not change its result.
#[tokio::test(flavor = "multi_thread")]
async fn delayed_storage_requests_do_not_change_the_result() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let delay = Duration::from_millis(300);
    for (op, key) in [
        (Op::Get, "head.json"),
        (Op::Put, "/entries/"),
        (Op::PutFile, "/packs/"),
        (Op::Put, "head.json"),
    ] {
        env.fault.add_rule(op, key, Action::Delay(delay), Some(1));
    }
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    let start = std::time::Instant::now();
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert!(start.elapsed() >= delay * 3);
    assert_eq!(env.stored_refs().await["refs/heads/main"], c2);
    check_history(&env, &[(c2, PushResult::Accepted)]).await;
}
