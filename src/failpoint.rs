//! Named points where tests stop a server in the middle of an operation.
//!
//! Only for tests. A server process reads `RUSTGIT_FAILPOINTS` at start, for
//! example `RUSTGIT_FAILPOINTS=push.after-cas=abort`, which ends the process
//! at that point exactly as a crash does. In-process tests pause the
//! operation forever instead: from the storage service's view, a server that
//! never continues is a stopped server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

pub const ENV: &str = "RUSTGIT_FAILPOINTS";

/// Push procedure, in order.
pub const PUSH_BEFORE_UPLOAD: &str = "push.before-upload";
pub const PUSH_AFTER_UPLOAD: &str = "push.after-upload";
pub const PUSH_AFTER_ENTRY: &str = "push.after-entry";
pub const PUSH_AFTER_PREPARE: &str = "push.after-prepare";
pub const PUSH_AFTER_CAS: &str = "push.after-cas";
pub const PUSH_AFTER_LOCAL_COMMIT: &str = "push.after-local-commit";
pub const PUSH_POINTS: &[&str] = &[
    PUSH_BEFORE_UPLOAD,
    PUSH_AFTER_UPLOAD,
    PUSH_AFTER_ENTRY,
    PUSH_AFTER_PREPARE,
    PUSH_AFTER_CAS,
    PUSH_AFTER_LOCAL_COMMIT,
];

/// Cache reconstruction, before the new cache replaces the old one.
pub const REBUILD_BEFORE_REPLACE: &str = "rebuild.before-replace";

#[derive(Clone, Debug)]
pub enum FailAction {
    /// End the process immediately, without cleanup.
    Abort,
    /// Stop the operation here forever and notify `reached`.
    Pause(Arc<Notify>),
}

#[derive(Debug, Default)]
pub struct Failpoints {
    points: Mutex<HashMap<String, FailAction>>,
}

impl Failpoints {
    /// Reads `name=abort` pairs, separated by commas, from the environment.
    pub fn from_env() -> Self {
        let f = Self::default();
        if let Ok(spec) = std::env::var(ENV) {
            for item in spec.split(',').filter(|s| !s.is_empty()) {
                match item.split_once('=') {
                    Some((name, "abort")) => f.set(name, FailAction::Abort),
                    _ => tracing::warn!(item, "ignored failpoint"),
                }
            }
            tracing::warn!(%spec, "failpoints are active");
        }
        f
    }

    pub fn set(&self, name: &str, action: FailAction) {
        self.points.lock().unwrap().insert(name.into(), action);
    }

    pub fn clear(&self) {
        self.points.lock().unwrap().clear();
    }

    pub async fn hit(&self, name: &str) {
        let action = self.points.lock().unwrap().get(name).cloned();
        match action {
            None => {}
            Some(FailAction::Abort) => {
                eprintln!("rustgit: failpoint {name}: abort");
                std::process::abort();
            }
            Some(FailAction::Pause(reached)) => {
                tracing::warn!(name, "failpoint: operation stopped");
                reached.notify_one();
                std::future::pending::<()>().await;
            }
        }
    }
}
