//! Access (IMPLEMENTATION_PLAN.md test group): unsafe Git configuration.
//! The server's Git processes ignore the environment and configuration of
//! the server process, and pushed objects cannot change Git's behavior.
//!
//! This file is its own test binary: it changes environment variables of
//! the process before any thread starts.

mod common;

use common::*;

#[test]
fn hostile_git_environment_and_configuration_have_no_effect() {
    let evil = tempfile::tempdir().unwrap();
    let marker = evil.path().join("ran");
    let script = evil.path().join("evil.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch {}\nexit 1\n", marker.display()),
    )
    .unwrap();
    let hooks = evil.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in [
        "pre-receive",
        "update",
        "post-receive",
        "post-update",
        "reference-transaction",
        "proc-receive",
    ] {
        std::fs::copy(&script, hooks.join(hook)).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    for f in std::fs::read_dir(&hooks)
        .unwrap()
        .chain(std::fs::read_dir(evil.path()).unwrap())
    {
        let p = f.unwrap().path();
        if p.is_file() {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let config = format!(
        "[core]\n\thooksPath = {h}\n\tfsmonitor = {s}\n\tsshCommand = {s}\n\
         [uploadpack]\n\tpackObjectsHook = {s}\n\tallowAnySHA1InWant = true\n\
         [receive]\n\tfsckObjects = false\n\tprocReceiveRefs = refs/nothing\n",
        h = hooks.display(),
        s = script.display()
    );
    std::fs::write(evil.path().join(".gitconfig"), &config).unwrap();
    std::fs::write(evil.path().join("config"), &config).unwrap();
    // SAFETY: no other thread runs yet; the runtime starts below.
    unsafe {
        std::env::set_var("HOME", evil.path());
        std::env::set_var("XDG_CONFIG_HOME", evil.path());
        std::env::set_var("GIT_CONFIG_GLOBAL", evil.path().join(".gitconfig"));
        std::env::set_var("GIT_CONFIG_SYSTEM", evil.path().join("config"));
        std::env::set_var(
            "GIT_CONFIG_PARAMETERS",
            format!("'core.hooksPath'='{}'", hooks.display()),
        );
        std::env::set_var("GIT_CONFIG_COUNT", "1");
        std::env::set_var("GIT_CONFIG_KEY_0", "uploadpack.packObjectsHook");
        std::env::set_var("GIT_CONFIG_VALUE_0", &script);
        std::env::set_var("GIT_DIR", evil.path());
        std::env::set_var("GIT_OBJECT_DIRECTORY", evil.path());
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let env = Env::new();
        let server = env.start().await;
        let alice = env.client("alice");
        let a = alice.init(&server.url(), "work").await;
        let c1 = alice.commit(&a, &[("f", b"1")], "one").await;
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
        let bob = env.client("bob");
        let b = bob.clone(&server.url_as("bob"), "copy").await;
        assert_eq!(bob.rev(&b, "HEAD").await, c1);

        // Receive checks stay on: a submodule name that leaves the
        // repository is rejected.
        std::fs::write(
            a.join(".gitmodules"),
            "[submodule \"../../escape\"]\n\tpath = x\n\turl = https://example.com/x\n",
        )
        .unwrap();
        alice.commit(&a, &[], "bad submodule").await;
        let err = alice.fails(&a, &["push", "origin", "main"]).await;
        assert!(err.contains("gitmodules"), "{err}");
        assert_eq!(env.stored_refs().await["refs/heads/main"], c1);

        // Ref changes still go through the controller: a rejected push
        // changes nothing even though the hostile configuration names
        // other hooks.
        assert_eq!(env.push_entries().await.len(), 1);
    });
    assert!(!marker.exists(), "a hostile hook or command ran");
}
