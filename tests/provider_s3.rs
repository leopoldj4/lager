//! Provider tests (IMPLEMENTATION_PLAN.md Stage 1 items 3-4, Stage 2 items
//! 1-2): the storage requirements, lost responses, several server processes,
//! and real process crashes against an S3-compatible service.
//!
//! These tests are kept apart from the simulated ones and are ignored by
//! default. Run them in the development shell, which provides SeaweedFS:
//!
//! ```text
//! cargo test --test provider_s3 -- --ignored
//! ```
//!
//! Without `RUSTGIT_TEST_S3_ENDPOINT`, the tests start `weed` (or
//! `$RUSTGIT_TEST_WEED`). To test a hosted provider, set
//! `RUSTGIT_TEST_S3_ENDPOINT`, `RUSTGIT_TEST_S3_BUCKET`,
//! `RUSTGIT_TEST_S3_REGION`, `RUSTGIT_TEST_S3_ACCESS_KEY`, and
//! `RUSTGIT_TEST_S3_SECRET_KEY`. A local test cannot prove another
//! provider's behavior: repeat these tests against each provider.

mod common;

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use rustgit::config::S3Config;
use rustgit::model::RepoId;
use rustgit::storage::fault::{Action, Op};
use rustgit::storage::{ObjectStoreBackend, Storage};

struct Provider {
    s3: S3Config,
}

static PROVIDER: OnceLock<Provider> = OnceLock::new();
static WEED_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static WEED_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Stops the SeaweedFS that this test binary started and removes its data.
extern "C" fn stop_weed() {
    let pid = WEED_PID.load(std::sync::atomic::Ordering::SeqCst);
    if pid > 0 {
        // SAFETY: signals and reaps the process this test binary started.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
    if let Some(dir) = WEED_DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A port that is free, with the port that SeaweedFS uses for gRPC (+10000).
fn free_port() -> u16 {
    loop {
        let p = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if p < 55000 && std::net::TcpListener::bind(("127.0.0.1", p + 10000)).is_ok() {
            return p;
        }
    }
}

fn provider() -> &'static Provider {
    PROVIDER.get_or_init(|| {
        let var = |k: &str| std::env::var(k).ok();
        if let Some(endpoint) = var("RUSTGIT_TEST_S3_ENDPOINT") {
            return Provider {
                s3: S3Config {
                    allow_http: endpoint.starts_with("http:"),
                    endpoint: Some(endpoint),
                    region: var("RUSTGIT_TEST_S3_REGION").unwrap_or_else(|| "us-east-1".into()),
                    bucket: var("RUSTGIT_TEST_S3_BUCKET").expect("RUSTGIT_TEST_S3_BUCKET"),
                    access_key_id: var("RUSTGIT_TEST_S3_ACCESS_KEY").expect("access key"),
                    secret_access_key: var("RUSTGIT_TEST_S3_SECRET_KEY").expect("secret key"),
                    virtual_hosted_style: false,
                    request_timeout_secs: 30,
                    ca_certificate: None,
                },
            };
        }
        let dir = tempfile::Builder::new()
            .prefix("rgs3")
            .tempdir_in("/tmp")
            .unwrap()
            .keep();
        std::fs::write(
            dir.join("s3.json"),
            r#"{"identities":[{"name":"t","credentials":[{"accessKey":"test","secretKey":"test-secret"}],"actions":["Admin","Read","Write","List","Tagging"]}]}"#,
        )
        .unwrap();
        let s3_port = free_port();
        let weed = var("RUSTGIT_TEST_WEED").unwrap_or_else(|| "weed".into());
        // Reaped by `stop_weed` when the test binary exits.
        #[allow(clippy::zombie_processes)]
        let child = std::process::Command::new(&weed)
            .arg("mini")
            .arg(format!("-dir={}", dir.join("data").display()))
            .args(["-ip=127.0.0.1", "-ip.bind=127.0.0.1", "-admin.ui=false", "-webdav=false"])
            .arg(format!("-s3.port={s3_port}"))
            .arg(format!("-master.port={}", free_port()))
            .arg(format!("-volume.port={}", free_port()))
            .arg(format!("-filer.port={}", free_port()))
            .arg(format!("-s3.config={}", dir.join("s3.json").display()))
            .arg("-bucket=rustgit-test")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("weed.log")).unwrap())
            .spawn()
            .unwrap_or_else(|e| panic!("start {weed} (run in the dev shell): {e}"));
        WEED_PID.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
        let _ = WEED_DIR.set(dir.clone());
        // SAFETY: registers a function without arguments that only sends a signal.
        unsafe { libc::atexit(stop_weed) };
        let p = Provider {
            s3: S3Config {
                endpoint: Some(format!("http://127.0.0.1:{s3_port}")),
                region: "us-east-1".into(),
                bucket: "rustgit-test".into(),
                access_key_id: "test".into(),
                secret_access_key: "test-secret".into(),
                allow_http: true,
                virtual_hosted_style: false,
                request_timeout_secs: 30,
                ca_certificate: None,
            },
        };
        // Wait until the service accepts conditional writes.
        let start = Instant::now();
        loop {
            let s = storage(&p.s3, "probe");
            // Its own thread: the caller may already run inside a runtime.
            let ok = std::thread::spawn(move || {
                tokio::runtime::Runtime::new().unwrap().block_on(async {
                    rustgit::storage::provider_check::run(&s)
                        .await
                        .iter()
                        .all(|(_, r)| r.is_ok())
                })
            })
            .join()
            .unwrap();
            if ok {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "SeaweedFS did not start; see {}",
                dir.join("weed.log").display()
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        p
    })
}

fn storage(s3: &S3Config, prefix: &str) -> Storage {
    Storage::new(Arc::new(ObjectStoreBackend::s3(s3).unwrap()), prefix)
}

fn unique_prefix() -> String {
    format!("t-{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an S3-compatible service; see the file header"]
async fn the_provider_meets_the_storage_requirements() {
    let p = provider();
    let results = rustgit::storage::provider_check::run(&storage(&p.s3, &unique_prefix())).await;
    assert!(results.len() >= 10);
    for (name, r) in results {
        assert!(r.is_ok(), "{name}: {r:?}");
    }
}

/// The provider applies a CAS whose answer is lost. The server resolves the
/// result from storage: one entry, and the client sees success.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an S3-compatible service; see the file header"]
async fn a_lost_cas_answer_from_the_provider_does_not_duplicate_the_push() {
    let p = provider();
    let env = Env::on_backend(
        Arc::new(ObjectStoreBackend::s3(&p.s3).unwrap()),
        &unique_prefix(),
    );
    let server = env.start_on(env.storage.clone(), |_| {}, "s3").await;
    let alice = env.client("alice");
    let a = alice.init(&server.url(), "work").await;
    alice
        .commit(&a, &[("big.bin", &noise(3, 20 * 1024 * 1024)[..])], "one")
        .await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let before = env.head().await.generation;

    let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
    env.fault
        .add_rule(Op::Put, "head.json", Action::LoseResponse, Some(1));
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    assert_eq!(env.head().await.generation, before + 1);
    check_history(&env, &[(c2, PushResult::Accepted)]).await;

    // Recovery from the provider after complete cache loss.
    server.stop_and_remove_cache();
    let fresh = env.start_on(env.storage.clone(), |_| {}, "s3-fresh").await;
    let bob = env.client("bob");
    let b = bob.clone(&fresh.url_as("bob"), "copy").await;
    assert_eq!(
        std::fs::read(b.join("big.bin")).unwrap(),
        noise(3, 20 * 1024 * 1024)
    );
}

/// One `rustgit serve` process with its own cache and configuration.
struct Process {
    child: tokio::process::Child,
    url: String,
}

impl Process {
    async fn start(
        dir: &Path,
        name: &str,
        prefix: &str,
        repo: RepoId,
        failpoints: &str,
    ) -> Process {
        let p = provider();
        let port = free_port();
        let cache = dir.join(name);
        let config = dir.join(format!("{name}.toml"));
        let s3 = &p.s3;
        std::fs::write(
            &config,
            format!(
                r#"
[server]
listen = "127.0.0.1:{port}"
id = "{name}"
cache_dir = "{cache}"
git_binary = "{git}"

[storage]
prefix = "{prefix}"

[storage.s3]
endpoint = "{endpoint}"
region = "{region}"
bucket = "{bucket}"
access_key_id = "{key}"
secret_access_key = "{secret}"
allow_http = {allow_http}

[limits]
min_free_disk_bytes = 0
unknown_result_deadline_secs = 2

[[repos]]
name = "{REPO}"
id = "{repo}"

[[users]]
name = "alice"
password = "{pw}"
access = {{ "*" = "force" }}
"#,
                cache = cache.display(),
                git = server_git().display(),
                endpoint = s3.endpoint.as_deref().unwrap(),
                region = s3.region,
                bucket = s3.bucket,
                key = s3.access_key_id,
                secret = s3.secret_access_key,
                allow_http = s3.allow_http,
                pw = password("alice"),
            ),
        )
        .unwrap();
        let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rustgit"))
            .arg("-c")
            .arg(&config)
            .arg("serve")
            .env("RUSTGIT_FAILPOINTS", failpoints)
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join(format!("{name}.log"))).unwrap())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let url = format!("http://127.0.0.1:{port}");
        let start = Instant::now();
        while reqwest::get(format!("{url}/_rustgit/health"))
            .await
            .is_err()
        {
            assert!(start.elapsed() < Duration::from_secs(30), "{name} starts");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Process { child, url }
    }

    fn repo_url(&self) -> String {
        self.url
            .replace("http://", &format!("http://alice:{}@", password("alice")))
            + &format!("/{REPO}.git")
    }

    async fn exit_code(&mut self) -> Option<i32> {
        tokio::time::timeout(Duration::from_secs(30), self.child.wait())
            .await
            .ok()
            .and_then(|s| s.ok())
            .map(|s| {
                use std::os::unix::process::ExitStatusExt;
                s.code().or(s.signal().map(|n| -n)).unwrap_or(0)
            })
    }
}

/// Three server processes with separate caches share the provider.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an S3-compatible service; see the file header"]
async fn three_server_processes_share_one_provider() {
    let p = provider();
    let prefix = unique_prefix();
    let env = Env::on_backend(Arc::new(ObjectStoreBackend::s3(&p.s3).unwrap()), &prefix);
    let dir = env.path("processes");
    std::fs::create_dir_all(&dir).unwrap();
    let mut servers = Vec::new();
    for name in ["a", "b", "c"] {
        servers.push(Process::start(&dir, name, &prefix, env.repo, "").await);
    }
    let alice = env.client("alice");
    let bob = env.client("bob");
    let a = alice.init(&servers[0].repo_url(), "work").await;
    alice.commit(&a, &[("f", b"base")], "base").await;
    alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
    let b = bob.clone(&servers[1].repo_url(), "work").await;

    // A read on each server includes the push that another one accepted.
    for i in 0..3 {
        let c = alice
            .commit(&a, &[("f", format!("{i}").as_bytes())], "x")
            .await;
        alice
            .ok(
                &a,
                &["push", "--quiet", &servers[i].repo_url(), "HEAD:main"],
            )
            .await;
        for s in &servers {
            assert_eq!(alice.ls_remote(&s.repo_url()).await["refs/heads/main"], c);
        }
    }
    // Competing pushes from the same old value on two processes.
    bob.ok(&b, &["pull", "--quiet", "--ff-only"]).await;
    let ca = alice.commit(&a, &[("f", b"alice")], "a").await;
    let cb = bob.commit(&b, &[("f", b"bob")], "b").await;
    let (url_a, url_c) = (servers[0].repo_url(), servers[2].repo_url());
    let (args_a, args_c) = (["push", &url_a, "HEAD:main"], ["push", &url_c, "HEAD:main"]);
    let (ra, rb) = tokio::join!(
        push_result(&alice, &a, &args_a),
        push_result(&bob, &b, &args_c),
    );
    assert!(
        (ra == PushResult::Accepted) ^ (rb == PushResult::Accepted),
        "{ra:?} {rb:?}"
    );
    check_history(&env, &[(ca, ra), (cb, rb)]).await;
    for s in &servers {
        assert_eq!(
            alice.ls_remote(&s.repo_url()).await,
            env.stored_refs().await
        );
    }
}

/// A server process crashes at each side of the CAS. Before it, nothing
/// changes; after it, the push is retained and a restart serves it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an S3-compatible service; see the file header"]
async fn a_server_process_crash_before_or_after_the_cas_loses_no_accepted_push() {
    for (point, accepted) in [("push.after-entry", false), ("push.after-cas", true)] {
        let p = provider();
        let prefix = unique_prefix();
        let env = Env::on_backend(Arc::new(ObjectStoreBackend::s3(&p.s3).unwrap()), &prefix);
        let dir = env.path("processes");
        std::fs::create_dir_all(&dir).unwrap();
        let normal = Process::start(&dir, "setup", &prefix, env.repo, "").await;
        let alice = env.client("alice");
        let a = alice.init(&normal.repo_url(), "work").await;
        let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;

        let mut crashing =
            Process::start(&dir, "crash", &prefix, env.repo, &format!("{point}=abort")).await;
        let c2 = alice.commit(&a, &[("f", b"2")], "two").await;
        let r = push_result(&alice, &a, &["push", &crashing.repo_url(), "main"]).await;
        assert_ne!(r, PushResult::Accepted, "{point}");
        assert_eq!(
            crashing.exit_code().await,
            Some(-libc::SIGABRT),
            "{point}: crashed"
        );
        let expected = if accepted { &c2 } else { &c1 };
        assert_eq!(
            &env.stored_refs().await["refs/heads/main"],
            expected,
            "{point}"
        );

        // A restart on the crashed server's cache, and the other server.
        drop(crashing);
        let restarted = Process::start(&dir, "crash", &prefix, env.repo, "").await;
        for s in [&restarted, &normal] {
            assert_eq!(
                &alice.ls_remote(&s.repo_url()).await["refs/heads/main"],
                expected,
                "{point}"
            );
        }
        alice
            .ok(&a, &["push", "--quiet", &restarted.repo_url(), "main"])
            .await;
        check_history(&env, &[(c2, PushResult::Unknown)]).await;
    }
}
