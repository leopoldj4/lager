//! A storage backend with controlled failures. Use only for tests.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Mutex;

use super::backend::{Backend, BackendError, ByteStream, Fetched, ObjectInfo, WriteMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Get,
    Put,
    PutFile,
    List,
    Delete,
}

#[derive(Clone, Debug)]
pub enum Action {
    /// Fail without contacting the service.
    Fail,
    /// Report a conditional-write conflict without contacting the service.
    Conflict,
    /// Perform the operation, then report no answer (a lost response).
    LoseResponse,
    /// Wait, then perform the operation.
    Delay(Duration),
    /// Wait for the gate, then perform the operation.
    Gate(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>),
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub op: Op,
    pub key_contains: String,
    pub action: Action,
    /// Number of matching calls to affect. `None` means all.
    pub remaining: Option<usize>,
}

/// One recorded storage call, for request-order checks.
#[derive(Clone, Debug)]
pub struct Call {
    pub op: Op,
    pub key: String,
    pub outcome: String,
}

#[derive(Debug)]
pub struct FaultBackend {
    inner: Arc<dyn Backend>,
    rules: Mutex<Vec<Rule>>,
    unavailable: AtomicBool,
    calls: Mutex<Vec<Call>>,
}

impl FaultBackend {
    pub fn new(inner: Arc<dyn Backend>) -> Self {
        Self {
            inner,
            rules: Mutex::new(Vec::new()),
            unavailable: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn add_rule(&self, op: Op, key_contains: &str, action: Action, times: Option<usize>) {
        self.rules.lock().unwrap().push(Rule {
            op,
            key_contains: key_contains.into(),
            action,
            remaining: times,
        });
    }

    pub fn clear_rules(&self) {
        self.rules.lock().unwrap().clear();
    }

    /// Makes every operation fail as if the service were unreachable.
    pub fn set_unavailable(&self, v: bool) {
        self.unavailable.store(v, Ordering::SeqCst);
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn take_action(&self, op: Op, key: &str) -> Option<Action> {
        let mut rules = self.rules.lock().unwrap();
        let idx = rules
            .iter()
            .position(|r| r.op == op && key.contains(&r.key_contains) && r.remaining != Some(0))?;
        let rule = &mut rules[idx];
        if let Some(n) = rule.remaining.as_mut() {
            *n -= 1;
        }
        Some(rule.action.clone())
    }

    fn record(&self, op: Op, key: &str, outcome: &str) {
        self.calls.lock().unwrap().push(Call {
            op,
            key: key.into(),
            outcome: outcome.into(),
        });
    }

    async fn run<T, F>(&self, op: Op, key: &str, f: F) -> Result<T, BackendError>
    where
        F: std::future::Future<Output = Result<T, BackendError>>,
    {
        if self.unavailable.load(Ordering::SeqCst) {
            self.record(op, key, "unavailable");
            return Err(BackendError::Unavailable(
                "injected: service unavailable".into(),
            ));
        }
        let action = self.take_action(op, key);
        match action {
            Some(Action::Fail) => {
                self.record(op, key, "injected-failure");
                Err(BackendError::Unavailable("injected failure".into()))
            }
            Some(Action::Conflict) => {
                self.record(op, key, "injected-conflict");
                Err(BackendError::Conflict)
            }
            Some(Action::LoseResponse) => {
                let r = f.await;
                self.record(op, key, &format!("lost-response({})", outcome(&r)));
                Err(BackendError::Unavailable("injected: response lost".into()))
            }
            Some(Action::Delay(d)) => {
                tokio::time::sleep(d).await;
                let r = f.await;
                self.record(op, key, outcome(&r));
                r
            }
            Some(Action::Gate(reached, release)) => {
                reached.notify_one();
                release.notified().await;
                let r = f.await;
                self.record(op, key, outcome(&r));
                r
            }
            None => {
                let r = f.await;
                self.record(op, key, outcome(&r));
                r
            }
        }
    }
}

fn outcome<T>(r: &Result<T, BackendError>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(BackendError::NotFound) => "not-found",
        Err(BackendError::NotModified) => "not-modified",
        Err(BackendError::Conflict) => "conflict",
        Err(BackendError::Unavailable(_)) => "unavailable",
        Err(BackendError::Other(_)) => "error",
    }
}

#[async_trait]
impl Backend for FaultBackend {
    async fn get(&self, key: &str, if_none_match: Option<&str>) -> Result<Fetched, BackendError> {
        self.run(Op::Get, key, self.inner.get(key, if_none_match))
            .await
    }
    async fn get_stream(&self, key: &str) -> Result<ByteStream, BackendError> {
        self.run(Op::Get, key, self.inner.get_stream(key)).await
    }
    async fn size(&self, key: &str) -> Result<u64, BackendError> {
        self.run(Op::Get, key, self.inner.size(key)).await
    }
    async fn put(
        &self,
        key: &str,
        data: Bytes,
        mode: WriteMode,
    ) -> Result<Option<String>, BackendError> {
        self.run(Op::Put, key, self.inner.put(key, data, mode))
            .await
    }
    async fn put_file(&self, key: &str, path: &Path) -> Result<(), BackendError> {
        self.run(Op::PutFile, key, self.inner.put_file(key, path))
            .await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>, BackendError> {
        self.run(Op::List, prefix, self.inner.list(prefix)).await
    }
    async fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.run(Op::Delete, key, self.inner.delete(key)).await
    }
}
