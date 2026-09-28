//! Access checks and Git smart HTTP details.

mod common;

use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn clients_need_credentials_and_repository_permission() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

    let other = env.client("other");
    // No credentials: the private repository is not readable.
    let err = other
        .fails(&other.home, &["ls-remote", &server.anonymous_url()])
        .await;
    assert!(err.contains("could not read Username"), "{err}");
    // Wrong password.
    let bad = server.url_as("bob").replace("bob-secret", "wrong");
    other.fails(&other.home, &["ls-remote", &bad]).await;
    // No access: the repository looks absent.
    let err = other
        .fails(&other.home, &["ls-remote", &server.url_as("dave")])
        .await;
    assert!(err.contains("not found") || err.contains("404"), "{err}");
    // Read access: fetch works, push does not.
    let carol = env.client("carol");
    let c = carol.clone(&server.url_as("carol"), "copy").await;
    assert_eq!(carol.rev(&c, "HEAD").await, c1);
    carol.commit(&c, &[("f", b"carol")], "carol").await;
    let err = carol.fails(&c, &["push", "origin", "main"]).await;
    assert!(err.contains("403"), "{err}");
    assert_eq!(env.stored_refs().await["refs/heads/main"], c1);
    assert_eq!(env.push_entries().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn force_pushes_and_tag_moves_need_force_permission() {
    let env = Env::new();
    let server = env.start().await;
    let bob = env.client("bob");
    let b = bob.init(&server.url_as("bob"), "work").await;
    let c1 = bob.commit(&b, &[("f", b"1")], "one").await;
    let c2 = bob.commit(&b, &[("f", b"2")], "two").await;
    bob.ok(&b, &["tag", "v1", &c1]).await;
    bob.ok(&b, &["push", "--quiet", "origin", "main", "v1"])
        .await;

    // A non-fast-forward update and a tag move from a writer are rejected.
    bob.ok(&b, &["reset", "--quiet", "--hard", &c1]).await;
    bob.commit(&b, &[("f", b"3")], "rewrite").await;
    let err = bob.fails(&b, &["push", "--force", "origin", "main"]).await;
    assert!(err.contains("force permission"), "{err}");
    bob.ok(&b, &["tag", "-f", "v1", &c2]).await;
    let err = bob.fails(&b, &["push", "--force", "origin", "v1"]).await;
    assert!(err.contains("force permission"), "{err}");
    assert_eq!(
        env.stored_refs().await,
        [
            ("refs/heads/main".to_string(), c2.clone()),
            ("refs/tags/v1".to_string(), c1.clone())
        ]
        .into()
    );

    // The same pushes from a user with force permission are accepted, and
    // the log keeps the client's exact expected old values.
    let alice = env.client("alice");
    let a = alice.clone(&server.url_as("alice"), "work").await;
    alice.ok(&a, &["reset", "--quiet", "--hard", &c1]).await;
    let c4 = alice.commit(&a, &[("f", b"4")], "alice rewrite").await;
    alice
        .ok(&a, &["push", "--quiet", "--force", "origin", "main"])
        .await;
    alice.ok(&a, &["tag", "-f", "v1", &c2]).await;
    alice
        .ok(&a, &["push", "--quiet", "--force", "origin", "v1"])
        .await;
    let pushes = env.push_entries().await;
    let force = &pushes[pushes.len() - 2];
    assert_eq!(force.refs[0].old.as_ref().unwrap().as_str(), c2);
    assert_eq!(force.refs[0].new.as_ref().unwrap().as_str(), c4);
    assert_eq!(
        env.stored_refs().await,
        [
            ("refs/heads/main".to_string(), c4),
            ("refs/tags/v1".to_string(), c2)
        ]
        .into()
    );
}

/// Objects of a rejected push are in the server's cache, but a fetch must
/// not return them.
#[tokio::test(flavor = "multi_thread")]
async fn objects_of_a_rejected_push_are_not_fetchable() {
    let env = Env::new();
    let server = env.start().await;
    let bob = env.client("bob");
    let b = bob.init(&server.url_as("bob"), "work").await;
    let c1 = bob.commit(&b, &[("f", b"1")], "one").await;
    bob.ok(&b, &["push", "--quiet", "origin", "main"]).await;
    bob.ok(&b, &["reset", "--quiet", "--hard", "HEAD"]).await;
    bob.ok(&b, &["checkout", "--quiet", "--orphan", "other"])
        .await;
    let secret = bob
        .commit(&b, &[("secret", b"rejected contents")], "rejected")
        .await;
    // Rejected: bob may not rewrite main.
    bob.fails(&b, &["push", "--force", "origin", "other:main"])
        .await;

    let carol = env.client("carol");
    let c = carol.clone(&server.url_as("carol"), "copy").await;
    assert_eq!(carol.rev(&c, "HEAD").await, c1);
    for version in ["2", "1", "0"] {
        let v = format!("protocol.version={version}");
        let out = carol.run(&c, &["-c", &v, "fetch", "origin", &secret]).await;
        assert!(
            !out.status.success(),
            "rejected commit was fetchable with {v}"
        );
    }
    assert!(
        carol
            .run(&c, &["cat-file", "-e", &secret])
            .await
            .status
            .code()
            != Some(0)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn http_responses_use_git_content_types_and_forbid_caching() {
    let env = Env::new();
    let server = env.start().await;
    let http = reqwest::Client::new();
    let base = server.base();

    let r = http
        .get(format!(
            "{base}/{REPO}.git/info/refs?service=git-upload-pack"
        ))
        .basic_auth("carol", Some(password("carol")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers()["content-type"],
        "application/x-git-upload-pack-advertisement"
    );
    assert!(
        r.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-cache")
    );
    let body = r.bytes().await.unwrap();
    assert!(body.starts_with(b"001e# service=git-upload-pack\n0000"));

    // Receive-pack advertisement needs write access.
    let r = http
        .get(format!(
            "{base}/{REPO}.git/info/refs?service=git-receive-pack"
        ))
        .basic_auth("carol", Some(password("carol")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // The dumb protocol is not served.
    let r = http
        .get(format!("{base}/{REPO}.git/info/refs"))
        .basic_auth("carol", Some(password("carol")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // A wrong request content type is rejected before Git runs.
    let r = http
        .post(format!("{base}/{REPO}.git/git-upload-pack"))
        .basic_auth("carol", Some(password("carol")))
        .header("content-type", "application/octet-stream")
        .body("0000")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 415);

    // Unknown encodings are rejected.
    let r = http
        .post(format!("{base}/{REPO}.git/git-upload-pack"))
        .basic_auth("carol", Some(password("carol")))
        .header("content-type", "application/x-git-upload-pack-request")
        .header("content-encoding", "br")
        .body("0000")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 415);
}

#[tokio::test(flavor = "multi_thread")]
async fn paths_outside_the_repository_table_are_not_served() {
    let env = Env::new();
    let server = env.start().await;
    for path in [
        "/../demo.git/info/refs?service=git-upload-pack",
        "/demo.git/../demo.git/info/refs?service=git-upload-pack",
        "/demo.git/HEAD",
        "/demo.git/config",
        "/demo.git/objects/info/packs",
        "/.git/info/refs?service=git-upload-pack",
        "/%2e%2e/demo.git/info/refs?service=git-upload-pack",
    ] {
        let status = raw_get(&server, path, "admin").await;
        assert_eq!(status, 404, "{path}");
    }
    let http = reqwest::Client::new();
    let base = server.base();
    // An unknown repository looks the same as a forbidden one to a user.
    let r = http
        .get(format!(
            "{base}/nothere.git/info/refs?service=git-upload-pack"
        ))
        .basic_auth("carol", Some(password("carol")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    // Anonymous clients get an authentication challenge, not a hint.
    let r = http
        .get(format!(
            "{base}/nothere.git/info/refs?service=git-upload-pack"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(r.headers().contains_key("www-authenticate"));
}

/// A client that disconnects in the middle of a push leaves no change.
#[tokio::test(flavor = "multi_thread")]
async fn a_push_request_cut_off_by_the_client_changes_nothing() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await;

    // A request body that promises more data than it sends.
    use tokio::io::AsyncWriteExt;
    let mut s = tokio::net::TcpStream::connect(server.running.addr)
        .await
        .unwrap();
    let auth = base64_basic("alice", &password("alice"));
    let zero = "0".repeat(40);
    let cmd = format!("{zero} {c1} refs/heads/cut\0report-status\n");
    let pkt = format!("{:04x}{cmd}", cmd.len() + 4);
    let head = format!(
        "POST /{REPO}.git/git-receive-pack HTTP/1.1\r\nHost: x\r\n\
         Authorization: Basic {auth}\r\n\
         Content-Type: application/x-git-receive-pack-request\r\n\
         Content-Length: 100000\r\n\r\n{pkt}0000PACK"
    );
    s.write_all(head.as_bytes()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    drop(s);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    assert_eq!(env.head().await.revision, before.revision);
    assert_eq!(alice.ls_remote(&server.url()).await.len(), 1);
    // The server still accepts pushes.
    alice
        .ok(&a, &["push", "--quiet", "origin", "main:refs/heads/after"])
        .await;
    assert_eq!(env.stored_refs().await.len(), 2);
}

fn base64_basic(user: &str, pw: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pw}"))
}

/// Sends the path exactly as given; HTTP client libraries normalize it.
async fn raw_get(server: &Server, path: &str, user: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(server.running.addr)
        .await
        .unwrap();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Basic {}\r\nConnection: close\r\n\r\n",
        base64_basic(user, &password(user))
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out);
    text.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// Measurements (ARCHITECTURE.md §11): per step, never per repository.
#[tokio::test(flavor = "multi_thread")]
async fn measurements_cover_push_steps_storage_use_and_git_work() {
    let env = Env::new();
    let server = env.start().await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice
        .commit(&a, &[("f", &noise(1, 50_000)[..])], "one")
        .await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let fresh = env.start().await;
    env.client("bob").clone(&fresh.url_as("bob"), "copy").await;

    let text = |s: &Server| {
        let url = format!("{}/_rustgit/metrics", s.base());
        async move { reqwest::get(url).await.unwrap().text().await.unwrap() }
    };
    let value = |text: &str, name: &str| -> f64 {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .unwrap_or_else(|| panic!("{name} missing"))
            .parse()
            .unwrap()
    };
    let writer = text(&server).await;
    assert_eq!(value(&writer, "rustgit_pushes_accepted_total"), 1.0);
    assert_eq!(value(&writer, "rustgit_storage_pack_uploads_total"), 1.0);
    assert!(value(&writer, "rustgit_storage_uploaded_bytes_total") > 50_000.0);
    assert!(value(&writer, "rustgit_receive_pack_requests_total") >= 1.0);
    for step in ["push_read_head", "push_upload", "push_cas", "push_total"] {
        for q in ["0.5", "0.95", "0.99"] {
            assert!(
                writer.contains(&format!(
                    "rustgit_step_seconds{{step=\"{step}\",quantile=\"{q}\"}}"
                )),
                "{step} {q}"
            );
        }
    }
    let reader = text(&fresh).await;
    assert_eq!(value(&reader, "rustgit_caches"), 1.0);
    assert_eq!(value(&reader, "rustgit_cache_rebuilds_total"), 1.0);
    assert_eq!(value(&reader, "rustgit_storage_pack_downloads_total"), 1.0);
    assert!(value(&reader, "rustgit_upload_pack_requests_total") >= 1.0);
    assert!(value(&reader, "rustgit_git_cpu_seconds_total") > 0.0);
    for t in [&writer, &reader] {
        assert!(!t.contains(&env.repo.to_string()) && !t.contains(REPO));
    }
}
