//! The minimal operations required from a storage service.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use object_store::path::Path as StorePath;
use object_store::{
    GetOptions, ObjectStore, ObjectStoreExt, PutMode, PutOptions, PutPayload, UpdateVersion,
    WriteMultipart,
};
use tokio::io::AsyncReadExt;

/// Errors from a backend. Conditional-write conflicts are separate from
/// connection failures, and connection failures on writes mean "unknown".
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("object not found")]
    NotFound,
    #[error("object not modified")]
    NotModified,
    #[error("conditional write conflict")]
    Conflict,
    /// The service did not answer. For a write, the result is unknown.
    #[error("storage service unavailable: {0}")]
    Unavailable(String),
    #[error("storage error: {0}")]
    Other(String),
}

#[derive(Clone, Debug)]
pub enum WriteMode {
    /// Create only if the object does not exist.
    Create,
    /// Replace only if the current ETag equals this value.
    Update(String),
}

#[derive(Clone, Debug)]
pub struct Fetched {
    pub data: Bytes,
    pub etag: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub last_modified_ms: u64,
}

pub type ByteStream = BoxStream<'static, Result<Bytes, BackendError>>;

#[async_trait]
pub trait Backend: Send + Sync + std::fmt::Debug {
    async fn get(&self, key: &str, if_none_match: Option<&str>) -> Result<Fetched, BackendError>;
    async fn get_stream(&self, key: &str) -> Result<ByteStream, BackendError>;
    async fn size(&self, key: &str) -> Result<u64, BackendError>;
    /// Returns the new ETag.
    async fn put(
        &self,
        key: &str,
        data: Bytes,
        mode: WriteMode,
    ) -> Result<Option<String>, BackendError>;
    /// Stores a complete file with limited memory. The key must be content
    /// addressed: an existing object with the same key has the same contents.
    async fn put_file(&self, key: &str, path: &Path) -> Result<(), BackendError>;
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>, BackendError>;
    async fn delete(&self, key: &str) -> Result<(), BackendError>;
}

/// Size of parts for large uploads, and of the read buffer.
pub const PART_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct ObjectStoreBackend {
    store: Arc<dyn ObjectStore>,
}

impl ObjectStoreBackend {
    pub fn new(store: Arc<dyn ObjectStore>) -> Self {
        Self { store }
    }

    /// An S3-compatible service. Conditional puts use `If-Match` and
    /// `If-None-Match`. Client retries are disabled so that a lost response
    /// reaches the caller as an unknown result instead of a false conflict.
    pub fn s3(cfg: &crate::config::S3Config) -> anyhow::Result<Self> {
        use object_store::aws::{AmazonS3Builder, S3ConditionalPut};
        use object_store::{ClientOptions, RetryConfig};
        let mut client = ClientOptions::new()
            .with_timeout(Duration::from_secs(cfg.request_timeout_secs))
            .with_connect_timeout(Duration::from_secs(5))
            .with_allow_http(cfg.allow_http);
        if let Some(ca) = &cfg.ca_certificate {
            let pem = std::fs::read(ca)?;
            client = client.with_root_certificate(object_store::Certificate::from_pem(&pem)?);
        }
        let retry = RetryConfig {
            max_retries: 0,
            ..Default::default()
        };
        let mut b = AmazonS3Builder::new()
            .with_bucket_name(&cfg.bucket)
            .with_region(&cfg.region)
            .with_access_key_id(&cfg.access_key_id)
            .with_secret_access_key(&cfg.secret_access_key)
            .with_conditional_put(S3ConditionalPut::ETagMatch)
            .with_virtual_hosted_style_request(cfg.virtual_hosted_style)
            .with_client_options(client)
            .with_retry(retry);
        if let Some(ep) = &cfg.endpoint {
            b = b.with_endpoint(ep);
        }
        Ok(Self::new(Arc::new(b.build()?)))
    }
}

fn map_err(e: object_store::Error) -> BackendError {
    use object_store::Error as E;
    match e {
        E::NotFound { .. } => BackendError::NotFound,
        E::NotModified { .. } => BackendError::NotModified,
        E::AlreadyExists { .. } | E::Precondition { .. } => BackendError::Conflict,
        E::Generic { store, source } => {
            // Everything that is not a definite answer from the service is
            // treated as "no answer". A conditional write must then be
            // resolved from storage contents, never assumed to have failed.
            BackendError::Unavailable(format!("{store}: {source}"))
        }
        other => BackendError::Other(other.to_string()),
    }
}

#[async_trait]
impl Backend for ObjectStoreBackend {
    async fn get(&self, key: &str, if_none_match: Option<&str>) -> Result<Fetched, BackendError> {
        let opts = GetOptions {
            if_none_match: if_none_match.map(str::to_string),
            ..Default::default()
        };
        let res = self
            .store
            .get_opts(&StorePath::from(key), opts)
            .await
            .map_err(map_err)?;
        let etag = res.meta.e_tag.clone();
        let data = res.bytes().await.map_err(map_err)?;
        Ok(Fetched { data, etag })
    }

    async fn get_stream(&self, key: &str) -> Result<ByteStream, BackendError> {
        let res = self
            .store
            .get(&StorePath::from(key))
            .await
            .map_err(map_err)?;
        Ok(res.into_stream().map(|r| r.map_err(map_err)).boxed())
    }

    async fn size(&self, key: &str) -> Result<u64, BackendError> {
        Ok(self
            .store
            .head(&StorePath::from(key))
            .await
            .map_err(map_err)?
            .size)
    }

    async fn put(
        &self,
        key: &str,
        data: Bytes,
        mode: WriteMode,
    ) -> Result<Option<String>, BackendError> {
        let mode = match mode {
            WriteMode::Create => PutMode::Create,
            WriteMode::Update(etag) => PutMode::Update(UpdateVersion {
                e_tag: Some(etag),
                version: None,
            }),
        };
        let opts = PutOptions {
            mode,
            ..Default::default()
        };
        let res = self
            .store
            .put_opts(&StorePath::from(key), PutPayload::from_bytes(data), opts)
            .await
            .map_err(map_err)?;
        Ok(res.e_tag)
    }

    async fn put_file(&self, key: &str, path: &Path) -> Result<(), BackendError> {
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| BackendError::Other(e.to_string()))?;
        let len = file
            .metadata()
            .await
            .map_err(|e| BackendError::Other(e.to_string()))?
            .len();
        let loc = StorePath::from(key);
        if len <= PART_SIZE as u64 {
            let mut buf = Vec::with_capacity(len as usize);
            file.read_to_end(&mut buf)
                .await
                .map_err(|e| BackendError::Other(e.to_string()))?;
            self.store
                .put(&loc, PutPayload::from(buf))
                .await
                .map_err(map_err)?;
            return Ok(());
        }
        let upload = self.store.put_multipart(&loc).await.map_err(map_err)?;
        let mut w = WriteMultipart::new_with_chunk_size(upload, PART_SIZE);
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            // At most two parts in flight keeps memory use bounded.
            w.wait_for_capacity(2).await.map_err(map_err)?;
            let n = file
                .read(&mut buf)
                .await
                .map_err(|e| BackendError::Other(e.to_string()))?;
            if n == 0 {
                break;
            }
            w.write(&buf[..n]);
        }
        w.finish().await.map_err(map_err)?;
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>, BackendError> {
        let mut out = Vec::new();
        let p = StorePath::from(prefix);
        let mut s = self.store.list(Some(&p));
        while let Some(m) = s.next().await {
            let m = m.map_err(map_err)?;
            out.push(ObjectInfo {
                key: m.location.to_string(),
                size: m.size,
                last_modified_ms: m.last_modified.timestamp_millis().max(0) as u64,
            });
        }
        Ok(out)
    }

    async fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.store
            .delete(&StorePath::from(key))
            .await
            .map_err(map_err)
    }
}
