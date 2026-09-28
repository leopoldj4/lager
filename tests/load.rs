//! Stage 3 item 10 (IMPLEMENTATION_PLAN.md): small, medium, and large
//! repositories under combined read and write load, with measurements.
//!
//! Ignored by default. Run with a release build for meaningful numbers:
//!
//! ```text
//! cargo test --release --test load -- --ignored --nocapture
//! ```
//!
//! The requirements are environment variables, because they must be agreed
//! for each deployment. The defaults only reject clearly broken behavior:
//! `RUSTGIT_LOAD_SECS` (15), `RUSTGIT_LOAD_MAX_PUSH_P99_MS` (15000),
//! `RUSTGIT_LOAD_MAX_FETCH_P99_MS` (15000), `RUSTGIT_LOAD_MAX_RECOVERY_MS`
//! (60000), and `RUSTGIT_LOAD_MIN_OPS_PER_SEC` (0.5).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::*;

fn setting(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[(((v.len() - 1) as f64) * p).round() as usize]
}

struct Size {
    name: &'static str,
    files: usize,
    blob_bytes: usize,
}

const SIZES: &[Size] = &[
    Size {
        name: "small",
        files: 20,
        blob_bytes: 0,
    },
    Size {
        name: "medium",
        files: 1000,
        blob_bytes: 8 * 1024 * 1024,
    },
    Size {
        name: "large",
        files: 5000,
        blob_bytes: 64 * 1024 * 1024,
    },
];

struct Report {
    size: &'static str,
    servers: usize,
    pushes: Vec<f64>,
    fetches: Vec<f64>,
    secs: f64,
    recovery_ms: f64,
}

async fn run(size: &Size, servers_n: usize) -> Report {
    let env = Env::new();
    let mut servers = Vec::new();
    for _ in 0..servers_n {
        servers.push(env.start().await);
    }
    let setup = env.client("setup");
    let s = setup.init(&servers[0].url(), "work").await;
    let mut files: Vec<(String, Vec<u8>)> = (0..size.files)
        .map(|i| (format!("src/{}/{i}.txt", i % 50), text(20, &format!("{i}"))))
        .collect();
    if size.blob_bytes > 0 {
        files.push(("assets/blob.bin".into(), noise(1, size.blob_bytes)));
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), &d[..])).collect();
    setup.commit(&s, &refs, "initial").await;
    setup.ok(&s, &["push", "--quiet", "origin", "main"]).await;

    let duration = Duration::from_secs_f64(setting("RUSTGIT_LOAD_SECS", 15.0));
    let stop = Arc::new(AtomicBool::new(false));
    let writers: Vec<Client> = (0..3).map(|i| env.client(&format!("w{i}"))).collect();
    let readers: Vec<Client> = (0..4).map(|i| env.client(&format!("r{i}"))).collect();
    let mut wdirs = Vec::new();
    for (i, w) in writers.iter().enumerate() {
        let d = w.clone(&servers[i % servers_n].url(), "work").await;
        w.ok(&d, &["checkout", "--quiet", "-b", &format!("w{i}")])
            .await;
        wdirs.push(d);
    }
    let mut rdirs = Vec::new();
    for (i, r) in readers.iter().enumerate() {
        rdirs.push(
            r.clone(&servers[i % servers_n].url_as("carol"), "copy")
                .await,
        );
    }

    let start = Instant::now();
    let write = futures::future::join_all(writers.iter().enumerate().map(|(i, w)| {
        let (dir, stop, url) = (&wdirs[i], stop.clone(), servers[i % servers_n].url());
        async move {
            let mut times = Vec::new();
            let mut results = Vec::new();
            let mut n = 0;
            while !stop.load(Ordering::SeqCst) {
                n += 1;
                let c = w
                    .commit(
                        dir,
                        &[(&format!("w{i}/{n}.txt"), &text(5, &format!("{n}"))[..])],
                        "w",
                    )
                    .await;
                let t = Instant::now();
                let r =
                    push_result(w, dir, &["push", &url, &format!("HEAD:refs/heads/w{i}")]).await;
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                results.push((c, r));
            }
            (times, results)
        }
    }));
    let read = futures::future::join_all(readers.iter().enumerate().map(|(i, r)| {
        let (dir, stop) = (&rdirs[i], stop.clone());
        async move {
            let mut times = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                let t = Instant::now();
                r.ok(dir, &["fetch", "--quiet", "origin"]).await;
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            times
        }
    }));
    let timer = async {
        tokio::time::sleep(duration).await;
        stop.store(true, Ordering::SeqCst);
    };
    let (w, r, ()) = tokio::join!(write, read, timer);
    let secs = start.elapsed().as_secs_f64();
    let mut pushes = Vec::new();
    let mut results = Vec::new();
    for (t, res) in w {
        pushes.extend(t);
        results.extend(res);
    }
    let fetches: Vec<f64> = r.into_iter().flatten().collect();
    assert!(
        results.iter().all(|(_, r)| *r == PushResult::Accepted),
        "{}: pushes to separate branches all succeed",
        size.name
    );
    check_history(&env, &results).await;

    // Recovery: a server with an empty cache serves its first read.
    let t = Instant::now();
    let fresh = env.start().await;
    assert_eq!(setup.ls_remote(&fresh.url()).await, env.stored_refs().await);
    let recovery_ms = t.elapsed().as_secs_f64() * 1000.0;
    Report {
        size: size.name,
        servers: servers_n,
        pushes,
        fetches,
        secs,
        recovery_ms,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "load test; run explicitly (see the file header)"]
async fn repositories_of_three_sizes_meet_the_load_requirements() {
    let max_push = setting("RUSTGIT_LOAD_MAX_PUSH_P99_MS", 15000.0);
    let max_fetch = setting("RUSTGIT_LOAD_MAX_FETCH_P99_MS", 15000.0);
    let max_recovery = setting("RUSTGIT_LOAD_MAX_RECOVERY_MS", 60000.0);
    let min_rate = setting("RUSTGIT_LOAD_MIN_OPS_PER_SEC", 0.5);
    let mut reports = Vec::new();
    for size in SIZES {
        reports.push(run(size, 3).await);
    }
    // Capacity as servers are added.
    reports.push(run(&SIZES[1], 1).await);

    eprintln!(
        "\n{:<7} {:>7} {:>6} {:>9} {:>9} {:>9} {:>6} {:>9} {:>9} {:>8} {:>11}",
        "size",
        "servers",
        "pushes",
        "push p50",
        "push p95",
        "push p99",
        "reads",
        "read p50",
        "read p99",
        "ops/s",
        "recovery ms"
    );
    for r in &mut reports {
        let rate = (r.pushes.len() + r.fetches.len()) as f64 / r.secs;
        let (p50, p95, p99) = (
            percentile(&mut r.pushes, 0.5),
            percentile(&mut r.pushes, 0.95),
            percentile(&mut r.pushes, 0.99),
        );
        let (f50, f99) = (
            percentile(&mut r.fetches, 0.5),
            percentile(&mut r.fetches, 0.99),
        );
        eprintln!(
            "{:<7} {:>7} {:>6} {:>9.0} {:>9.0} {:>9.0} {:>6} {:>9.0} {:>9.0} {:>8.1} {:>11.0}",
            r.size,
            r.servers,
            r.pushes.len(),
            p50,
            p95,
            p99,
            r.fetches.len(),
            f50,
            f99,
            rate,
            r.recovery_ms
        );
        assert!(p99 <= max_push, "{}: push p99 {p99:.0} ms", r.size);
        assert!(f99 <= max_fetch, "{}: fetch p99 {f99:.0} ms", r.size);
        assert!(
            r.recovery_ms <= max_recovery,
            "{}: recovery {:.0} ms",
            r.size,
            r.recovery_ms
        );
        assert!(
            rate >= min_rate,
            "{}: {rate:.2} operations per second",
            r.size
        );
    }
}
