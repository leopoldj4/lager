//! Stage 1 check (IMPLEMENTATION_PLAN.md): large transfers stay within the
//! memory limit. The server streams request and response bodies with fixed
//! buffers, uploads packs in parts, and downloads them to disk, so its
//! memory does not grow with the size of a push or clone.
//!
//! This file is its own test binary: it measures the peak memory of the
//! process that runs the server.

mod common;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use common::*;
use rustgit::storage::backend::{ByteStream, Fetched};
use rustgit::storage::{Backend, BackendError, ObjectInfo, ObjectStoreBackend, Storage, WriteMode};

/// Packs on disk, everything else in memory: the storage service itself
/// must not count as server memory.
#[derive(Debug)]
struct PacksOnDisk {
    disk: ObjectStoreBackend,
    memory: ObjectStoreBackend,
}

impl PacksOnDisk {
    fn pick(&self, key: &str) -> &ObjectStoreBackend {
        if key.contains("/packs/") {
            &self.disk
        } else {
            &self.memory
        }
    }
}

#[async_trait]
impl Backend for PacksOnDisk {
    async fn get(&self, key: &str, inm: Option<&str>) -> Result<Fetched, BackendError> {
        self.pick(key).get(key, inm).await
    }
    async fn get_stream(&self, key: &str) -> Result<ByteStream, BackendError> {
        self.pick(key).get_stream(key).await
    }
    async fn size(&self, key: &str) -> Result<u64, BackendError> {
        self.pick(key).size(key).await
    }
    async fn put(
        &self,
        key: &str,
        data: Bytes,
        mode: WriteMode,
    ) -> Result<Option<String>, BackendError> {
        self.pick(key).put(key, data, mode).await
    }
    async fn put_file(&self, key: &str, path: &Path) -> Result<(), BackendError> {
        self.pick(key).put_file(key, path).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>, BackendError> {
        let mut all = self.memory.list(prefix).await?;
        all.extend(self.disk.list(prefix).await?);
        Ok(all)
    }
    async fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.pick(key).delete(key).await
    }
}

/// Peak resident memory of this process in bytes.
fn peak_rss() -> u64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `u` is a valid out pointer.
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) }, 0);
    let v = u.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        v
    } else {
        v * 1024
    }
}

/// Memory growth allowed for the complete test: a few upload parts and
/// buffers, independent of the transfer size.
const LIMIT: u64 = 64 * 1024 * 1024;
const SIZE: usize = 96 * 1024 * 1024;

#[test]
fn a_large_push_and_clone_stay_within_the_memory_limit() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let env = Env::new();
        let disk = env.path("storage");
        std::fs::create_dir_all(&disk).unwrap();
        let backend = PacksOnDisk {
            disk: ObjectStoreBackend::new(Arc::new(
                object_store::local::LocalFileSystem::new_with_prefix(&disk).unwrap(),
            )),
            memory: ObjectStoreBackend::in_memory(),
        };
        let storage = Storage::new(Arc::new(backend), "mem");
        let server = env.start_on(storage.clone(), |_| {}, "writer").await;
        let alice = env.client("alice");
        let a = alice.init(&server.url(), "work").await;
        // Warm up allocators and connection pools with a small transfer.
        alice.commit(&a, &[("small", b"x")], "small").await;
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
        env.client("warm")
            .clone(&server.url_as("carol"), "warm")
            .await;

        // The file is written by the client; only the transfer counts.
        std::fs::write(a.join("big.bin"), noise(42, SIZE)).unwrap();
        let tip = alice.commit(&a, &[], "big").await;
        let before = peak_rss();
        alice.ok(&a, &["push", "--quiet", "origin", "main"]).await;
        // Another server rebuilds its cache from storage and serves a clone.
        let reader = env.start_on(storage.clone(), |_| {}, "reader").await;
        let bob = env.client("bob");
        let b = bob.clone(&reader.url_as("bob"), "copy").await;
        let growth = peak_rss().saturating_sub(before);

        assert_eq!(bob.rev(&b, "HEAD").await, tip);
        assert_eq!(
            std::fs::metadata(b.join("big.bin")).unwrap().len(),
            SIZE as u64
        );
        let stored: u64 = stored_refs(&storage, env.repo).await.len() as u64;
        assert_eq!(stored, 1);
        assert!(
            growth < LIMIT,
            "server memory grew by {} MiB for a {} MiB transfer",
            growth >> 20,
            SIZE >> 20
        );
        eprintln!(
            "memory growth: {} MiB for a {} MiB push, rebuild, and clone",
            growth >> 20,
            SIZE >> 20
        );
    });
}
