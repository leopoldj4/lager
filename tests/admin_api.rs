//! The admin API behind the management interface: users in storage and the
//! repository list (docs/decisions/0004-users-in-storage.md).

mod common;

use std::time::Duration;

use common::*;
use rustgit::catalog::Catalog;
use rustgit::config::{CatalogKind, RepoConfig};
use rustgit::model::RepoId;
use serde_json::{Value, json};

/// A server with the storage catalog, users in storage, and repository
/// `demo` in the catalog. Lookups are not reused, so changes apply at once.
async fn start(env: &Env) -> Server {
    let server = env
        .start_with(
            |cfg| {
                cfg.repos.clear();
                cfg.catalog.kind = CatalogKind::Storage;
                cfg.catalog.cache_secs = 0;
            },
            "api",
        )
        .await;
    catalog(env)
        .create(&RepoConfig::new(REPO, RepoId::new()), "test")
        .await
        .unwrap();
    server
}

fn catalog(env: &Env) -> Catalog {
    Catalog::new(env.storage.clone(), Duration::ZERO)
}

struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    fn new(server: &Server) -> Api {
        Api {
            base: format!("{}/_rustgit/api", server.base()),
            http: reqwest::Client::new(),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        who: Option<(&str, &str)>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some((u, p)) = who {
            req = req.basic_auth(u, Some(p));
        }
        if let Some(b) = body {
            req = req
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn admin(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        self.call(method, path, Some(("admin", &password("admin"))), body)
            .await
    }
}

fn names(users: &Value) -> Vec<(&str, &str)> {
    users["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| (u["name"].as_str().unwrap(), u["source"].as_str().unwrap()))
        .collect()
}

/// A user created through the API can push with its access level; a changed
/// password and changed access apply to the next request, and a deleted user
/// can no longer sign in.
#[tokio::test(flavor = "multi_thread")]
async fn stored_users_are_created_changed_and_deleted_through_the_api() {
    use reqwest::Method;
    let env = Env::new();
    let server = start(&env).await;
    let api = Api::new(&server);
    let url = |p: &str| format!("http://erin:{p}@{}/{REPO}.git", server.running.addr);

    let (status, body) = api
        .admin(
            Method::POST,
            "/users",
            Some(
                json!({"name": "erin", "password": "first-password", "access": {"demo": "write"}}),
            ),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["source"], "storage");
    assert_eq!(body["updated_by"], "admin");
    assert!(
        body.get("password_hash").is_none(),
        "hashes are never returned"
    );
    let stored = rustgit::users::UserStore::new(env.storage.clone(), Duration::ZERO)
        .lookup("erin")
        .await
        .unwrap()
        .unwrap();
    assert!(stored.password_hash.starts_with("pbkdf2-sha256$"));
    assert!(!stored.password_hash.contains("first-password"));

    let erin = env.client("erin");
    let dir = erin.init(&url("first-password"), "work").await;
    let c1 = erin.commit(&dir, &[("f", b"1")], "one").await;
    erin.ok(&dir, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(
        env.client("carol").ls_remote(&server.url_as("carol")).await["refs/heads/main"],
        c1
    );

    // The same name cannot be created twice.
    let (status, _) = api
        .admin(
            Method::POST,
            "/users",
            Some(json!({"name": "erin", "password": "other-password"})),
        )
        .await;
    assert_eq!(status, 409);

    // A new password replaces the old one at once.
    let (status, _) = api
        .admin(
            Method::PATCH,
            "/users/erin",
            Some(json!({"password": "second-password"})),
        )
        .await;
    assert_eq!(status, 200);
    let err = erin
        .fails(&dir, &["ls-remote", &url("first-password")])
        .await;
    assert!(
        err.contains("Authentication failed") || err.contains("401"),
        "{err}"
    );
    erin.ok(
        &dir,
        &["remote", "set-url", "origin", &url("second-password")],
    )
    .await;
    erin.ok(&dir, &["fetch", "--quiet", "origin"]).await;

    // Read access no longer permits pushes.
    let (status, body) = api
        .admin(
            Method::PATCH,
            "/users/erin",
            Some(json!({"access": {"demo": "read"}})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["access"], json!({"demo": "read"}));
    erin.commit(&dir, &[("f", b"2")], "two").await;
    let err = erin.fails(&dir, &["push", "origin", "main"]).await;
    assert!(err.contains("403"), "{err}");
    assert_eq!(
        env.client("carol").ls_remote(&server.url_as("carol")).await["refs/heads/main"],
        c1
    );

    // A deleted user cannot sign in, and the name can be created again.
    let (status, _) = api.admin(Method::DELETE, "/users/erin", None).await;
    assert_eq!(status, 204);
    erin.fails(&dir, &["fetch", "origin"]).await;
    let (_, list) = api.admin(Method::GET, "/users", None).await;
    assert!(!names(&list).iter().any(|(n, _)| *n == "erin"));
    let (status, _) = api.admin(Method::DELETE, "/users/erin", None).await;
    assert_eq!(status, 404);
    let (status, _) = api
        .admin(
            Method::POST,
            "/users",
            Some(json!({"name": "erin", "password": "third-password"})),
        )
        .await;
    assert_eq!(status, 201);
    // Without access, the reused name sees no repository.
    let err = erin
        .fails(&dir, &["ls-remote", &url("third-password")])
        .await;
    assert!(err.contains("not found") || err.contains("404"), "{err}");
}

/// Only users with admin access to every repository can use the API, and
/// users from the configuration file cannot be changed through it.
#[tokio::test(flavor = "multi_thread")]
async fn the_api_needs_a_global_admin_and_keeps_configured_users() {
    use reqwest::Method;
    let env = Env::new();
    let server = start(&env).await;
    let api = Api::new(&server);

    let (status, _) = api.call(Method::GET, "/users", None, None).await;
    assert_eq!(status, 401);
    let (status, _) = api
        .call(Method::GET, "/users", Some(("admin", "wrong")), None)
        .await;
    assert_eq!(status, 401);
    let alice = ("alice", password("alice"));
    let alice = Some((alice.0, alice.1.as_str()));
    let (status, me) = api.call(Method::GET, "/me", alice, None).await;
    assert_eq!(
        (status, me),
        (200, json!({"name": "alice", "admin": false}))
    );
    for (method, path) in [(Method::GET, "/users"), (Method::GET, "/repos")] {
        let (status, _) = api.call(method, path, alice, None).await;
        assert_eq!(status, 403, "{path}");
    }
    let (status, _) = api
        .call(
            Method::POST,
            "/users",
            alice,
            Some(json!({"name": "mallory", "password": "long-enough-password", "access": {"*": "admin"}})),
        )
        .await;
    assert_eq!(status, 403);

    let (status, me) = api.admin(Method::GET, "/me", None).await;
    assert_eq!((status, me), (200, json!({"name": "admin", "admin": true})));
    let (status, list) = api.admin(Method::GET, "/users", None).await;
    assert_eq!(status, 200);
    assert_eq!(list["managed"], true);
    assert_eq!(
        names(&list),
        [
            ("admin", "config"),
            ("alice", "config"),
            ("bob", "config"),
            ("carol", "config"),
            ("dave", "config")
        ]
    );

    for (body, expected) in [
        (
            json!({"name": "a:b", "password": "long-enough-password"}),
            400,
        ),
        (json!({"name": "frank", "password": "short"}), 400),
        (
            json!({"name": "frank", "password": "long-enough-password", "access": {"../x": "read"}}),
            400,
        ),
        (
            json!({"name": "frank", "password": "long-enough-password", "access": {"demo": "owner"}}),
            422,
        ),
        (
            json!({"name": "bob", "password": "long-enough-password"}),
            409,
        ),
    ] {
        let (status, answer) = api.admin(Method::POST, "/users", Some(body.clone())).await;
        assert_eq!(status, expected, "{body} {answer}");
    }
    let (status, _) = api
        .admin(Method::PATCH, "/users/bob", Some(json!({"access": {}})))
        .await;
    assert_eq!(status, 409);
    let (status, _) = api.admin(Method::DELETE, "/users/admin", None).await;
    assert_eq!(status, 409);
    let (_, list) = api.admin(Method::GET, "/users", None).await;
    assert_eq!(names(&list).len(), 5, "nothing was created");

    // A stored global admin can manage users too, but not delete itself.
    let (status, _) = api
        .admin(
            Method::POST,
            "/users",
            Some(json!({"name": "grace", "password": "long-enough-password", "access": {"*": "admin"}})),
        )
        .await;
    assert_eq!(status, 201);
    let grace = Some(("grace", "long-enough-password"));
    let (status, _) = api.call(Method::GET, "/users", grace, None).await;
    assert_eq!(status, 200);
    let (status, _) = api.call(Method::DELETE, "/users/grace", grace, None).await;
    assert_eq!(status, 409);
}

/// The repository list shows active catalog repositories with the number of
/// accepted changes; deleted repositories are not listed.
#[tokio::test(flavor = "multi_thread")]
async fn the_repository_list_shows_active_repositories_and_their_changes() {
    use reqwest::Method;
    let env = Env::new();
    let server = start(&env).await;
    let api = Api::new(&server);
    let catalog = catalog(&env);
    catalog
        .create(&RepoConfig::new("team/app", RepoId::new()), "test")
        .await
        .unwrap();
    catalog
        .create(&RepoConfig::new("old", RepoId::new()), "test")
        .await
        .unwrap();
    catalog.delete("old", "test").await.unwrap();

    let alice = env.client("alice");
    let dir = alice.init(&server.url(), "work").await;
    alice.commit(&dir, &[("f", b"1")], "one").await;
    alice.ok(&dir, &["push", "--quiet", "origin", "main"]).await;
    alice.commit(&dir, &[("f", b"2")], "two").await;
    alice.ok(&dir, &["push", "--quiet", "origin", "main"]).await;

    let (status, repos) = api.admin(Method::GET, "/repos", None).await;
    assert_eq!(status, 200);
    let repos = repos.as_array().unwrap();
    let summary: Vec<_> = repos
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap(),
                r["generation"].as_u64().unwrap(),
            )
        })
        .collect();
    let created = |name: &str| summary.iter().find(|(n, _)| *n == name).unwrap().1;
    assert_eq!(
        summary.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        ["demo", "team/app"]
    );
    assert_eq!(
        created("demo"),
        created("team/app") + 2,
        "two pushes to demo"
    );
    assert_eq!(repos[0]["source"], "catalog");
    assert_eq!(repos[0]["default_branch"], "main");
}
