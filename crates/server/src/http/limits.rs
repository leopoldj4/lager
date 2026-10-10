//! Customer limits: simultaneous Git processes, request rate, and received
//! bytes for the repositories of one customer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use worktree_core::config::CustomerConfig;

/// A fixed time window with a counter.
struct Window {
    len: Duration,
    start: Instant,
    used: u64,
}

impl Window {
    fn new(len: Duration) -> Self {
        Self {
            len,
            start: Instant::now(),
            used: 0,
        }
    }

    fn take(&mut self, n: u64, limit: u64) -> bool {
        if self.start.elapsed() >= self.len {
            self.start = Instant::now();
            self.used = 0;
        }
        if self.used.saturating_add(n) > limit {
            return false;
        }
        self.used += n;
        true
    }
}

struct Customer {
    cfg: CustomerConfig,
    processes: Option<Arc<Semaphore>>,
    requests: Mutex<Window>,
    received: Mutex<Window>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct LimitExceeded(pub &'static str);

#[derive(Default)]
pub struct CustomerLimits {
    customers: HashMap<String, Arc<Customer>>,
}

/// Charges received bytes against a customer's budget. Returns false when
/// the budget is exhausted.
pub type ByteBudget = Arc<dyn Fn(u64) -> bool + Send + Sync>;

impl CustomerLimits {
    pub fn new(cfgs: &[CustomerConfig]) -> Self {
        Self {
            customers: cfgs
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        Arc::new(Customer {
                            cfg: c.clone(),
                            processes: c.max_git_processes.map(|n| Arc::new(Semaphore::new(n))),
                            requests: Mutex::new(Window::new(Duration::from_secs(60))),
                            received: Mutex::new(Window::new(Duration::from_secs(3600))),
                        }),
                    )
                })
                .collect(),
        }
    }

    fn get(&self, customer: Option<&str>) -> Option<&Arc<Customer>> {
        self.customers.get(customer?)
    }

    /// Counts one Git request.
    pub fn admit_request(&self, customer: Option<&str>) -> Result<(), LimitExceeded> {
        let Some(c) = self.get(customer) else {
            return Ok(());
        };
        match c.cfg.max_requests_per_minute {
            Some(limit) if !c.requests.lock().unwrap().take(1, limit) => {
                Err(LimitExceeded("request rate"))
            }
            _ => Ok(()),
        }
    }

    /// A process slot of the customer. Never waits: other customers' work
    /// must not queue behind a busy customer.
    pub fn process_permit(
        &self,
        customer: Option<&str>,
    ) -> Result<Option<OwnedSemaphorePermit>, LimitExceeded> {
        let Some(sem) = self.get(customer).and_then(|c| c.processes.clone()) else {
            return Ok(None);
        };
        sem.try_acquire_owned()
            .map(Some)
            .map_err(|_| LimitExceeded("simultaneous Git processes"))
    }

    /// Reserves `expected` received bytes before a push starts, and returns
    /// the budget that charges bytes beyond the reservation during receipt.
    pub fn receive_budget(
        &self,
        customer: Option<&str>,
        expected: u64,
    ) -> Result<Option<ByteBudget>, LimitExceeded> {
        let Some(c) = self.get(customer).cloned() else {
            return Ok(None);
        };
        let Some(limit) = c.cfg.max_receive_bytes_per_hour else {
            return Ok(None);
        };
        if !c.received.lock().unwrap().take(expected, limit) {
            return Err(LimitExceeded("received bytes per hour"));
        }
        let reserved = Arc::new(std::sync::atomic::AtomicU64::new(expected));
        Ok(Some(Arc::new(move |n: u64| {
            use std::sync::atomic::Ordering;
            // Bytes within the reservation are already counted.
            let left = reserved.load(Ordering::SeqCst);
            let covered = left.min(n);
            reserved.fetch_sub(covered, Ordering::SeqCst);
            n == covered || c.received.lock().unwrap().take(n - covered, limit)
        })))
    }
}
