//! Readiness for load balancers: the storage service answers, the change
//! bus is connected, and the server is not stopping.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bus::Bus;
use worktree_core::storage::Storage;

/// A storage answer is reused for this long, so that frequent health checks
/// from several load balancers cost at most one storage request each time.
const FRESH: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotReady {
    Draining,
    StorageUnavailable,
    BusUnavailable,
}

pub struct Health {
    storage: Storage,
    bus: Arc<Bus>,
    draining: AtomicBool,
    last: Mutex<Option<(Instant, bool)>>,
    /// One probe at a time; waiting checks use its answer.
    probe: tokio::sync::Mutex<()>,
}

impl Health {
    pub fn new(storage: Storage, bus: Arc<Bus>) -> Self {
        Self {
            storage,
            bus,
            draining: AtomicBool::new(false),
            last: Mutex::new(None),
            probe: tokio::sync::Mutex::new(()),
        }
    }

    pub fn start_draining(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    pub async fn check(&self) -> Result<(), NotReady> {
        if self.is_draining() {
            return Err(NotReady::Draining);
        }
        if !self.bus.healthy() {
            return Err(NotReady::BusUnavailable);
        }
        if self.storage_ok().await {
            Ok(())
        } else {
            Err(NotReady::StorageUnavailable)
        }
    }

    fn fresh(&self) -> Option<bool> {
        match *self.last.lock().unwrap() {
            Some((at, ok)) if at.elapsed() < FRESH => Some(ok),
            _ => None,
        }
    }

    async fn storage_ok(&self) -> bool {
        if let Some(ok) = self.fresh() {
            return ok;
        }
        let _probe = self.probe.lock().await;
        if let Some(ok) = self.fresh() {
            return ok;
        }
        let ok = match tokio::time::timeout(PROBE_TIMEOUT, self.storage.probe()).await {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "health check: storage does not answer");
                false
            }
            Err(_) => {
                tracing::warn!("health check: storage request timed out");
                false
            }
        };
        let was = self.last.lock().unwrap().replace((Instant::now(), ok));
        if ok && was.is_some_and(|(_, ok)| !ok) {
            tracing::info!("health check: storage answers again");
        }
        ok
    }
}
