//! Git service processes connected to HTTP bodies with limited buffers.
//!
//! Every process is supervised: on cancellation (client disconnect) it is
//! stopped, its exit is awaited, and its pipes are drained.

use std::any::Any;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use async_compression::tokio::bufread::GzipDecoder;
use axum::body::Body;
use bytes::Bytes;
use futures::{Stream, TryStreamExt};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{ChildStdout, Command};
use tokio::sync::oneshot;
use tokio_util::io::{ReaderStream, StreamReader};

/// Values that must live until the response body ends: read views, cache
/// instances, process permits, disk reservations, operation tokens.
#[derive(Default)]
pub struct Guards(Vec<Box<dyn Any + Send + Sync>>);

impl Guards {
    pub fn hold<T: Any + Send + Sync>(&mut self, v: T) {
        self.0.push(Box::new(v));
    }
}

const BUF: usize = 64 * 1024;

pub struct Input {
    pub body: Body,
    pub gzip: bool,
    pub max_bytes: u64,
    /// Customer traffic budget, charged as the body arrives.
    pub budget: Option<super::limits::ByteBudget>,
}

/// Starts `cmd` with the request body as stdin and returns its stdout as a
/// response body stream.
pub fn spawn_service(
    mut cmd: Command,
    input: Input,
    guards: Guards,
    label: &'static str,
    metrics: Arc<crate::metrics::Metrics>,
) -> std::io::Result<Body> {
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(false);
    let mut child = cmd.spawn()?;
    metrics.inc("git_processes_started");
    let mut stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let over_limit = Arc::new(AtomicBool::new(false));
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Request body -> stdin, decompressed, limited.
    let over = over_limit.clone();
    let writer = tokio::spawn(async move {
        let stream = input.body.into_data_stream().map_err(std::io::Error::other);
        let reader = StreamReader::new(stream);
        let mut reader: Pin<Box<dyn AsyncRead + Send>> = if input.gzip {
            Box::pin(GzipDecoder::new(BufReader::with_capacity(BUF, reader)))
        } else {
            Box::pin(reader)
        };
        use tokio::io::AsyncWriteExt;
        let mut buf = vec![0u8; BUF];
        let mut total = 0u64;
        let result = loop {
            let n = match reader.read(&mut buf).await {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) => break Err(e),
            };
            total += n as u64;
            let within_budget = input.budget.as_ref().is_none_or(|b| b(n as u64));
            if total > input.max_bytes || !within_budget {
                over.store(true, Ordering::SeqCst);
                break Ok(());
            }
            if let Err(e) = stdin.write_all(&buf[..n]).await {
                break Err(e);
            }
        };
        drop(stdin);
        result
    });

    // Diagnostics: keep a bounded amount, drain the rest.
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut kept = 0usize;
        while let Ok(Some(line)) = lines.next_line().await {
            if kept < 50 {
                tracing::debug!(service = label, "{line}");
                kept += 1;
            }
        }
    });

    // Supervisor.
    let over = over_limit.clone();
    tokio::spawn(async move {
        let mut cancel_rx = cancel_rx;
        let cancelled = tokio::select! {
            status = child.wait() => {
                log_exit(label, status);
                false
            }
            r = &mut cancel_rx => r.is_ok(),
        };
        if cancelled || over.load(Ordering::SeqCst) {
            // The client went away or sent too much: stop the process.
            let _ = child.start_kill();
        }
        // The process no longer reads its input; a stalled client must not
        // keep this task alive.
        writer.abort();
        let status = child.wait().await;
        metrics.inc("git_processes_exited");
        if cancelled {
            log_exit(label, status);
        }
        let _ = err_task.await;
    });

    let body = ProcessBody {
        inner: ReaderStream::with_capacity(stdout, BUF),
        _guards: guards,
        cancel: Some(cancel_tx),
        over_limit,
    };
    Ok(Body::from_stream(body))
}

fn log_exit(label: &str, status: std::io::Result<std::process::ExitStatus>) {
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => tracing::debug!(service = label, status = %s, "git service exited with failure"),
        Err(e) => tracing::warn!(service = label, error = %e, "waiting for git service failed"),
    }
}

struct ProcessBody {
    inner: ReaderStream<ChildStdout>,
    _guards: Guards,
    cancel: Option<oneshot::Sender<()>>,
    over_limit: Arc<AtomicBool>,
}

impl Stream for ProcessBody {
    type Item = std::io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.over_limit.load(Ordering::SeqCst) {
            return Poll::Ready(Some(Err(std::io::Error::other(
                "request body over the limit",
            ))));
        }
        let r = Pin::new(&mut self.inner).poll_next(cx);
        if let Poll::Ready(None) = r {
            // Normal end: let the supervisor wait for the exit.
            self.cancel.take();
        }
        r
    }
}

impl Drop for ProcessBody {
    fn drop(&mut self) {
        if let Some(c) = self.cancel.take() {
            let _ = c.send(());
        }
    }
}
