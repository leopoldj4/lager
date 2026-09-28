//! Git objects, refs, and HTTP protocol details (IMPLEMENTATION_PLAN.md
//! test groups) that standard clients do not produce on their own: these
//! tests send raw receive-pack requests.

mod common;

use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn progress_messages_and_final_results_reach_the_client() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["branch", "topic"]).await;
    let out = alice
        .run(&a, &["push", "--porcelain", "origin", "main", "topic"])
        .await;
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for r in ["refs/heads/main", "refs/heads/topic"] {
        assert!(
            stdout.lines().any(|l| l.starts_with('*') && l.contains(r)),
            "{stdout}"
        );
    }
    // The server's progress line names the generation and the request UUID
    // that the log entry records.
    let line = stderr
        .lines()
        .find(|l| l.contains("rustgit: accepted as generation 1"))
        .unwrap_or_else(|| panic!("{stderr}"));
    let entry = env.push_entries().await.pop().unwrap();
    assert!(line.contains(&entry.request.to_string()), "{line}");

    // A rejected change: the final result names the ref and the reason.
    alice.commit(&a, &[("f", b"2")], "two").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    alice
        .ok(&a, &["reset", "--quiet", "--hard", "HEAD~1"])
        .await;
    alice.commit(&a, &[("f", b"3")], "other").await;
    let out = alice
        .run(&a, &["push", "--porcelain", "origin", "main"])
        .await;
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with('!') && l.contains("refs/heads/main")),
        "{stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_branch_can_be_created_again() {
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
    alice.ok(&a, &["push", "--quiet", "origin", ":x"]).await;
    assert!(!env.stored_refs().await.contains_key("refs/heads/x"));
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    alice
        .ok(&a, &["push", "--quiet", "origin", "main:refs/heads/x"])
        .await;
    let stored = env.stored_refs().await;
    assert_eq!(stored["refs/heads/x"], c2);
    let x: Vec<_> = env
        .push_entries()
        .await
        .into_iter()
        .flat_map(|e| e.refs)
        .filter(|u| u.name.as_str() == "refs/heads/x")
        .map(|u| (u.old.map(|o| o.to_string()), u.new.map(|o| o.to_string())))
        .collect();
    assert_eq!(
        x,
        vec![(None, Some(c1.clone())), (Some(c1), None), (None, Some(c2)),]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_commands_in_one_request_reject_the_push() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    let before = env.head().await;

    let pack = alice.pack(&a, &format!("{c2}\n^{c1}\n")).await;
    let body = receive_request(
        &[(ZERO, &c2, "refs/heads/dup"), (ZERO, &c2, "refs/heads/dup")],
        &pack,
    );
    let (status, report) = raw_receive_pack(&server, "alice", body).await;
    assert_eq!(status, 200);
    assert!(report.contains("ng refs/heads/dup"), "{report}");
    assert!(!report.contains("ok refs/heads/dup"), "{report}");
    assert_eq!(env.head().await.revision, before.revision);
}

/// A pack whose commit needs a parent that the server does not have.
#[tokio::test(flavor = "multi_thread")]
async fn a_pack_with_missing_dependencies_is_rejected() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    let c3 = alice.commit(&a, &[("g", b"3")], "three").await;
    let before = env.head().await;

    let pack = alice.pack(&a, &format!("{c3}\n^{c2}\n")).await;
    let body = receive_request(&[(ZERO, &c3, "refs/heads/x")], &pack);
    let (_, report) = raw_receive_pack(&server, "alice", body).await;
    assert!(report.contains("ng refs/heads/x"), "{report}");
    assert_eq!(env.head().await.revision, before.revision);
    assert!(!env.stored_refs().await.contains_key("refs/heads/x"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_pack_in_a_push_is_rejected() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice
        .commit(&a, &[("f", &noise(1, 20_000)[..])], "one")
        .await;
    let before = env.head().await;
    let mut pack = alice.pack(&a, &format!("{c1}\n")).await;
    let middle = pack.len() / 2;
    for b in &mut pack[middle..middle + 64] {
        *b ^= 0x5a;
    }
    let body = receive_request(&[(ZERO, &c1, "refs/heads/main")], &pack);
    let (_, report) = raw_receive_pack(&server, "alice", body).await;
    assert!(!report.contains("unpack ok"), "{report}");
    assert!(!report.contains("ok refs/heads/main"), "{report}");
    assert_eq!(env.head().await.revision, before.revision);
    assert!(env.stored_refs().await.is_empty());
}

/// Only branches and tags are supported. Other namespaces are rejected, even
/// for administrators.
#[tokio::test(flavor = "multi_thread")]
async fn refs_outside_branches_and_tags_are_rejected() {
    let env = Env::new();
    let server = env.start().await;
    let admin = env.client("admin");
    let a = admin.init(&server.url_as("admin"), "work").await;
    let c1 = admin.commit(&a, &[("f", b"1")], "one").await;
    admin.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    for target in [
        "refs/notes/x",
        "refs/pull/1/head",
        "refs/meta/config",
        "refs/heads/x.lock",
    ] {
        let spec = format!("{c1}:{target}");
        let err = admin.fails(&a, &["push", "origin", &spec]).await;
        assert!(
            err.contains("only refs/heads/ and refs/tags/") || err.contains("invalid ref"),
            "{target}: {err}"
        );
    }
    assert_eq!(env.stored_refs().await.len(), 1);
}
