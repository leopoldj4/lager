//! Stage 3 item 4-5 (IMPLEMENTATION_PLAN.md): ref permissions, salted
//! password hashes, an identity service, the repository catalog, and TLS.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use common::*;
use rustgit::catalog::Catalog;
use rustgit::config::{Access, CatalogKind, IdentityConfig, RefRule, RepoConfig};
use rustgit::model::{RepoId, RepoState};

fn rule(pattern: &str, update: Access, force: Access, delete: Access) -> RefRule {
    RefRule {
        pattern: pattern.into(),
        update,
        force,
        delete,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ref_rules_protect_selected_refs() {
    use Access::*;
    let env = Env::new();
    let server = env
        .start_with(
            |cfg| {
                cfg.repos[0].ref_rules = vec![
                    rule("refs/heads/main", Write, Admin, Admin),
                    rule("refs/heads/release/*", Admin, Admin, Admin),
                    rule("refs/tags/*", Write, Admin, Admin),
                ];
            },
            "rules",
        )
        .await;
    let (bob, alice, admin) = (env.client("bob"), env.client("alice"), env.client("admin"));
    let b = bob.init(&server.url_as("bob"), "work").await;
    let c1 = bob.commit(&b, &[("f", b"1")], "one").await;
    let c2 = bob.commit(&b, &[("f", b"2")], "two").await;
    bob.ok(&b, &["push", "--quiet", "origin", "main"]).await;
    bob.ok(&b, &["tag", "v1", &c1]).await;
    bob.ok(&b, &["push", "--quiet", "origin", "v1"]).await;

    // Alice has force permission, but main needs admin for rewrites.
    let a = alice.clone(&server.url_as("alice"), "work").await;
    alice.ok(&a, &["reset", "--quiet", "--hard", &c1]).await;
    alice.commit(&a, &[("f", b"alice")], "rewrite").await;
    let err = alice
        .fails(&a, &["push", "--force", "origin", "main"])
        .await;
    assert!(err.contains("ref rule: force needs admin access"), "{err}");
    let err = alice.fails(&a, &["push", "origin", ":main"]).await;
    assert!(
        err.contains("ref rule: deletion needs admin access"),
        "{err}"
    );
    // Other branches keep the default rules.
    alice
        .ok(&a, &["push", "--quiet", "origin", "HEAD:refs/heads/topic"])
        .await;
    alice.ok(&a, &["reset", "--quiet", "--hard", &c2]).await;
    alice
        .ok(
            &a,
            &[
                "push",
                "--quiet",
                "--force",
                "origin",
                "HEAD:refs/heads/topic",
            ],
        )
        .await;
    bob.ok(&b, &["push", "--quiet", "origin", ":topic"]).await;

    // Release branches need admin even for creation.
    let err = bob
        .fails(&b, &["push", "origin", "main:refs/heads/release/1"])
        .await;
    assert!(
        err.contains("ref rule: creation needs admin access"),
        "{err}"
    );
    let ad = admin.clone(&server.url_as("admin"), "work").await;
    admin
        .ok(
            &ad,
            &["push", "--quiet", "origin", "main:refs/heads/release/1"],
        )
        .await;
    // Tags: creation for writers, moves for admins.
    alice.ok(&a, &["tag", "-f", "v1", &c2]).await;
    let err = alice.fails(&a, &["push", "--force", "origin", "v1"]).await;
    assert!(err.contains("ref rule: force needs admin access"), "{err}");
    admin.ok(&ad, &["tag", "-f", "v1", &c2]).await;
    admin
        .ok(&ad, &["push", "--quiet", "--force", "origin", "v1"])
        .await;
    // An admin may rewrite main.
    admin.ok(&ad, &["reset", "--quiet", "--hard", &c1]).await;
    let rewrite = admin.commit(&ad, &[("f", b"admin")], "admin rewrite").await;
    admin
        .ok(&ad, &["push", "--quiet", "--force", "origin", "main"])
        .await;

    assert_eq!(
        env.stored_refs().await,
        [
            ("refs/heads/main".to_string(), rewrite),
            ("refs/heads/release/1".to_string(), c2.clone()),
            ("refs/tags/v1".to_string(), c2),
        ]
        .into()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn salted_password_hashes_authenticate_git_clients() {
    let env = Env::new();
    let hash = rustgit::http::password::PasswordHash::new("erin-token", 10_000).to_string();
    let server = env
        .start_with(
            |cfg| {
                let u = cfg.users.iter_mut().find(|u| u.name == "bob").unwrap();
                u.password = None;
                u.password_hash = Some(hash);
            },
            "hashes",
        )
        .await;
    let bob = env.client("bob");
    let url = |pw: &str| format!("http://bob:{pw}@{}/{REPO}.git", server.running.addr);
    let b = bob.init(&url("erin-token"), "work").await;
    let c = bob.commit(&b, &[("f", b"1")], "one").await;
    bob.ok(&b, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], c);
    bob.fails(&bob.home, &["ls-remote", &url(&password("bob"))])
        .await;
}

/// Identities and access levels come from an external service. Answers are
/// reused for a short time; an unreachable service fails requests.
#[tokio::test(flavor = "multi_thread")]
async fn an_identity_service_decides_identity_and_access() {
    let calls = Arc::new(AtomicUsize::new(0));
    let down = Arc::new(AtomicBool::new(false));
    let service = {
        let (calls, down) = (calls.clone(), down.clone());
        axum::Router::new().route(
            "/check",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                    let (calls, down) = (calls.clone(), down.clone());
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        if down.load(Ordering::SeqCst) {
                            return (axum::http::StatusCode::BAD_GATEWAY, String::new());
                        }
                        assert_eq!(headers["authorization"], "Bearer idp-secret");
                        let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let access = match (req["username"].as_str(), req["password"].as_str()) {
                            (Some("ivan"), Some("token-1")) => r#"{"demo":"write"}"#,
                            (Some("vera"), Some("token-2")) => r#"{"*":"read"}"#,
                            _ => return (axum::http::StatusCode::UNAUTHORIZED, String::new()),
                        };
                        let user = req["username"].as_str().unwrap();
                        (
                            axum::http::StatusCode::OK,
                            format!(r#"{{"user":"{user}","access":{access}}}"#),
                        )
                    }
                },
            ),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let idp = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });

    let env = Env::new();
    let server = env
        .start_with(
            |cfg| {
                cfg.users.clear();
                cfg.identity = Some(IdentityConfig {
                    url: format!("http://{idp}/check"),
                    token: Some("idp-secret".into()),
                    cache_secs: 60,
                    timeout_secs: 5,
                    ca_certificate: None,
                });
            },
            "idp",
        )
        .await;
    let url = |u: &str, p: &str| format!("http://{u}:{p}@{}/{REPO}.git", server.running.addr);
    let ivan = env.client("ivan");
    let i = ivan.init(&url("ivan", "token-1"), "work").await;
    let c = ivan.commit(&i, &[("f", b"1")], "one").await;
    ivan.ok(&i, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(env.stored_refs().await["refs/heads/main"], c);
    assert_eq!(env.push_entries().await[0].user, "ivan");
    let answered = calls.load(Ordering::SeqCst);

    let vera = env.client("vera");
    let v = vera.clone(&url("vera", "token-2"), "copy").await;
    vera.commit(&v, &[("f", b"2")], "two").await;
    let err = vera.fails(&v, &["push", "origin", "main"]).await;
    assert!(err.contains("403"), "{err}");
    vera.fails(&vera.home, &["ls-remote", &url("vera", "wrong")])
        .await;
    assert!(calls.load(Ordering::SeqCst) > answered);

    // Cached answers keep known clients working briefly; new ones fail.
    down.store(true, Ordering::SeqCst);
    assert_eq!(
        ivan.ls_remote(&url("ivan", "token-1")).await["refs/heads/main"],
        c
    );
    let err = ivan
        .fails(&ivan.home, &["ls-remote", &url("ivan", "token-other")])
        .await;
    assert!(err.contains("503"), "{err}");
}

/// Repository names from the catalog in storage. A reused name gets a new
/// repository; the deleted one is never served again.
#[tokio::test(flavor = "multi_thread")]
async fn repositories_in_the_storage_catalog_are_created_deleted_and_reused() {
    let env = Env::new();
    let server = env
        .start_with(
            |cfg| {
                cfg.repos.clear();
                cfg.catalog.kind = CatalogKind::Storage;
                cfg.catalog.cache_secs = 0;
            },
            "catalog",
        )
        .await;
    let catalog = Catalog::new(env.storage.clone(), std::time::Duration::ZERO);
    let created = catalog
        .create(&RepoConfig::new("team/app", RepoId::new()), "test")
        .await
        .unwrap();
    assert!(
        catalog
            .create(&RepoConfig::new("team/app", RepoId::new()), "test")
            .await
            .is_err(),
        "a name maps to one active repository"
    );
    let alice = env.client("alice");
    let a = alice
        .init(&server.url_for("alice", "team/app"), "work")
        .await;
    let old = alice.commit(&a, &[("f", b"old")], "old").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(
        stored_refs(&env.storage, created.id).await["refs/heads/main"],
        old
    );
    // The catalog key is a digest, never the client-supplied name.
    let key = catalog.key("team/app");
    assert!(!key.contains("team") && key.starts_with("test/catalog/"));

    catalog.delete("team/app", "test").await.unwrap();
    let err = alice
        .fails(
            &alice.home,
            &["ls-remote", &server.url_for("alice", "team/app")],
        )
        .await;
    assert!(err.contains("not found"), "{err}");
    assert_eq!(
        load_head(&env.storage, created.id).await.state,
        RepoState::Deleted
    );

    let reused = catalog
        .create(&RepoConfig::new("team/app", RepoId::new()), "test")
        .await
        .unwrap();
    assert_ne!(reused.id, created.id);
    assert!(
        alice
            .ls_remote(&server.url_for("alice", "team/app"))
            .await
            .is_empty()
    );
    let bob = env.client("bob");
    let b = bob.init(&server.url_for("bob", "team/app"), "empty").await;
    assert!(
        !bob.run(&b, &["fetch", "origin", &old])
            .await
            .status
            .success()
    );
    let names: Vec<_> = catalog
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, vec!["team/app"]);
}

/// With `catalog.create_on_push`, a push to a missing name creates the
/// repository if the user may write to that name. The first push chooses the
/// default branch when it does not contain `main`.
#[tokio::test(flavor = "multi_thread")]
async fn a_push_creates_a_missing_repository() {
    let env = Env::new();
    let server = env
        .start_with(
            |cfg| {
                cfg.repos.clear();
                cfg.catalog.kind = CatalogKind::Storage;
                cfg.catalog.cache_secs = 0;
                cfg.catalog.create_on_push = Some(Access::Write);
            },
            "create-on-push",
        )
        .await;
    let catalog = Catalog::new(env.storage.clone(), std::time::Duration::ZERO);
    let url = |user: &str| server.url_for(user, "team/new");

    // Reads never create; neither do users without write access.
    let bob = env.client("bob");
    let err = bob.fails(&bob.home, &["ls-remote", &url("bob")]).await;
    assert!(err.contains("not found"), "{err}");
    let carol = env.client("carol");
    let c = carol.init(&url("carol"), "work").await;
    carol.commit(&c, &[("f", b"carol")], "carol").await;
    let err = carol.fails(&c, &["push", "origin", "HEAD:trunk"]).await;
    assert!(err.contains("not found"), "{err}");
    assert!(catalog.record("team/new").await.unwrap().is_none());

    let b = bob.init(&url("bob"), "work").await;
    let first = bob.commit(&b, &[("f", b"1")], "one").await;
    let out = bob.run(&b, &["push", "origin", "HEAD:trunk"]).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("default branch is now trunk"), "{stderr}");
    let (record, _) = catalog.record("team/new").await.unwrap().unwrap();
    assert_eq!(record.state, RepoState::Active);
    assert_eq!(record.default_branch, "trunk");
    assert_eq!(
        stored_refs(&env.storage, record.repository).await["refs/heads/trunk"],
        first
    );

    // A clone checks out the pushed branch; later pushes change nothing else.
    let alice = env.client("alice");
    let a = alice.clone(&url("alice"), "clone").await;
    assert_eq!(alice.rev(&a, "HEAD").await, first);
    assert_eq!(
        alice.ok(&a, &["symbolic-ref", "--short", "HEAD"]).await,
        "trunk"
    );
    alice.commit(&a, &[("f", b"2")], "two").await;
    alice
        .ok(&a, &["push", "--quiet", "origin", "trunk", "HEAD:main"])
        .await;
    let (again, _) = catalog.record("team/new").await.unwrap().unwrap();
    assert_eq!(
        (again.repository, again.default_branch.as_str()),
        (record.repository, "trunk")
    );
    assert_eq!(server.metric("repositories_created_on_push"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_clients_connect_and_plain_http_is_refused() {
    let env = Env::new();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let (cert_path, key_path) = (env.path("tls.pem"), env.path("tls.key"));
    std::fs::write(&cert_path, cert.cert.pem()).unwrap();
    std::fs::write(&key_path, cert.signing_key.serialize_pem()).unwrap();
    let server = env
        .start_with(
            |cfg| {
                cfg.server.tls_certificate = Some(cert_path.clone());
                cfg.server.tls_private_key = Some(key_path.clone());
            },
            "tls",
        )
        .await;
    let port = server.running.addr.port();
    let url = format!(
        "https://alice:{}@localhost:{port}/{REPO}.git",
        password("alice")
    );
    let alice = env.client("alice");
    let ca = format!("http.sslCAInfo={}", cert_path.display());
    let a = alice.init(&url, "work").await;
    alice
        .ok(
            &a,
            &["config", "http.sslCAInfo", &cert_path.display().to_string()],
        )
        .await;
    let c = alice.commit(&a, &[("f", b"1")], "one").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let out = alice.ok(&alice.home, &["-c", &ca, "ls-remote", &url]).await;
    assert_eq!(parse_ls_remote(&out)["refs/heads/main"], c);
    // Without the certificate, or with plain HTTP, there is no access.
    alice.fails(&alice.home, &["ls-remote", &url]).await;
    let plain = format!(
        "http://alice:{}@localhost:{port}/{REPO}.git",
        password("alice")
    );
    alice.fails(&alice.home, &["ls-remote", &plain]).await;
}
