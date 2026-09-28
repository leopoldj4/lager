//! Stage 3 item 5 (IMPLEMENTATION_PLAN.md): diagnostic records keep request
//! identifiers and never contain credentials or repository contents
//! (ARCHITECTURE.md §11).
//!
//! This file is its own test binary: it installs the global log subscriber.

mod common;

use std::sync::{Arc, Mutex};

use base64::Engine;
use common::*;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn diagnostic_records_name_requests_but_not_credentials_or_contents() {
    let capture = Capture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .with_env_filter("rustgit=debug")
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (request, push_request) = rt.block_on(async {
        let env = Env::new();
        let server = env.start().await;
        let alice = env.client("alice");
        let a = alice.init(&server.url(), "work").await;
        alice
            .commit(
                &a,
                &[("secret.txt", b"TOP-SECRET-CONTENT-4711\n")],
                "secret",
            )
            .await;
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
        // A wrong password is also kept out of the records.
        alice
            .fails(
                &alice.home,
                &[
                    "ls-remote",
                    &server.url().replace("alice-secret", "wrong-pass-0815"),
                ],
            )
            .await;
        let r = reqwest::Client::new()
            .get(format!(
                "{}/{REPO}.git/info/refs?service=git-upload-pack",
                server.base()
            ))
            .basic_auth("carol", Some(password("carol")))
            .send()
            .await
            .unwrap();
        let id = r.headers()[rustgit::http::REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_string();
        (id, env.push_entries().await[0].request.to_string())
    });

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains(&request),
        "the response's request ID is recorded"
    );
    assert!(
        logs.lines()
            .any(|l| l.contains("push accepted") && l.contains(&push_request)),
        "the push's request UUID is recorded with its result"
    );
    let basic = base64::engine::general_purpose::STANDARD;
    for secret in [
        password("alice"),
        password("carol"),
        "wrong-pass-0815".to_string(),
        basic.encode(format!("alice:{}", password("alice"))),
        basic.encode(format!("carol:{}", password("carol"))),
        "TOP-SECRET-CONTENT-4711".to_string(),
    ] {
        assert!(!logs.contains(&secret), "the records contain {secret}");
    }
}
