//! Change events between servers over NATS
//! (docs/decisions/0012-nats-change-bus.md).
//!
//! After an accepted push, a server publishes the repository and its new
//! generation. Each server receives the events of all servers and passes
//! them, and its own, to local subscribers. Events are hints: a lost event
//! delays a cache update but never changes a read result, because every
//! request checks the storage head.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::sync::{broadcast, watch};

use crate::metrics::Metrics;
use worktree_core::config::BusConfig;
use worktree_core::model::RepoId;

/// Local subscribers that fall this far behind lose the oldest events.
const LOCAL_CAPACITY: usize = 1024;
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Change {
    pub repo: RepoId,
    pub generation: u64,
    /// The server that accepted the push, for diagnostic records. Server
    /// IDs need not be unique.
    pub server: String,
    /// Received from another server.
    #[serde(skip)]
    pub remote: bool,
}

pub struct Bus {
    self_id: String,
    nats: Option<Nats>,
    local: broadcast::Sender<Change>,
    metrics: Arc<Metrics>,
}

struct Nats {
    client: async_nats::Client,
    prefix: String,
    grace: Duration,
    link: Arc<Link>,
}

/// Connection state for the health check.
#[derive(Default)]
struct Link {
    ever_connected: AtomicBool,
    down_since: Mutex<Option<Instant>>,
}

impl Bus {
    /// Starts the connection in the background: the server starts while
    /// NATS is unavailable, and its health check fails until it connects.
    pub async fn connect(
        self_id: String,
        cfg: Option<&BusConfig>,
        metrics: Arc<Metrics>,
    ) -> anyhow::Result<Self> {
        let (local, _) = broadcast::channel(LOCAL_CAPACITY);
        let nats = match cfg {
            None => None,
            Some(cfg) => {
                let link = Arc::new(Link::default());
                let l = link.clone();
                let client = async_nats::ConnectOptions::new()
                    .name(format!("worktree-server {self_id}"))
                    .retry_on_initial_connect()
                    // Own events go to local subscribers directly.
                    .no_echo()
                    .event_callback(move |event| {
                        let l = l.clone();
                        async move {
                            match event {
                                async_nats::Event::Connected => {
                                    l.ever_connected.store(true, Ordering::SeqCst);
                                    *l.down_since.lock().unwrap() = None;
                                }
                                async_nats::Event::Disconnected => {
                                    l.down_since
                                        .lock()
                                        .unwrap()
                                        .get_or_insert_with(Instant::now);
                                }
                                _ => {}
                            }
                        }
                    })
                    .connect(cfg.url.as_str())
                    .await?;
                Some(Nats {
                    client,
                    prefix: worktree_core::limits::BUS_SUBJECT_PREFIX.into(),
                    grace: worktree_core::limits::BUS_HEALTH_GRACE,
                    link,
                })
            }
        };
        Ok(Self {
            self_id,
            nats,
            local,
            metrics,
        })
    }

    /// Announces an accepted push. Never waits for NATS.
    pub fn publish(&self, repo: RepoId, generation: u64) {
        let change = Change {
            repo,
            generation,
            server: self.self_id.clone(),
            remote: false,
        };
        let Some(n) = &self.nats else {
            let _ = self.local.send(change);
            return;
        };
        let subject = format!("{}.repo.{repo}.changed", n.prefix);
        let payload = serde_json::to_vec(&change).expect("serializes");
        let _ = self.local.send(change);
        let client = n.client.clone();
        let metrics = self.metrics.clone();
        tokio::spawn(async move {
            match tokio::time::timeout(PUBLISH_TIMEOUT, client.publish(subject, payload.into()))
                .await
            {
                Ok(Ok(())) => metrics.inc("bus_published"),
                Ok(Err(e)) => {
                    metrics.inc("bus_publish_failures");
                    tracing::warn!(%repo, error = %e, "change event not published");
                }
                Err(_) => {
                    metrics.inc("bus_publish_failures");
                    tracing::warn!(%repo, "change event not published: timeout");
                }
            }
        });
    }

    pub fn server_id(&self) -> &str {
        &self.self_id
    }

    /// Events of all servers, including this one.
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.local.subscribe()
    }

    /// Whether the NATS connection is up, or was down for less than the
    /// grace period. False before the first connection.
    pub fn healthy(&self) -> bool {
        let Some(n) = &self.nats else { return true };
        if n.client.connection_state() == async_nats::connection::State::Connected {
            n.link.ever_connected.store(true, Ordering::SeqCst);
            *n.link.down_since.lock().unwrap() = None;
            return true;
        }
        if !n.link.ever_connected.load(Ordering::SeqCst) {
            return false;
        }
        let since = *n
            .link
            .down_since
            .lock()
            .unwrap()
            .get_or_insert_with(Instant::now);
        since.elapsed() < n.grace
    }

    /// Passes events of other servers to local subscribers until the stop
    /// signal. NATS subscriptions survive reconnects, and NATS does not
    /// return this server's own events.
    pub async fn receive(self: Arc<Self>, mut stop: watch::Receiver<bool>) -> anyhow::Result<()> {
        let Some(n) = &self.nats else { return Ok(()) };
        let mut sub = n
            .client
            .subscribe(format!("{}.repo.*.changed", n.prefix))
            .await?;
        loop {
            let msg = tokio::select! {
                m = sub.next() => m,
                _ = crate::server::stopped(&mut stop) => break,
            };
            let Some(msg) = msg else { break };
            let mut change: Change = match serde_json::from_slice(&msg.payload) {
                Ok(c) => c,
                Err(e) => {
                    self.metrics.inc("bus_invalid_events");
                    tracing::warn!(subject = %msg.subject, error = %e, "invalid change event");
                    continue;
                }
            };
            change.remote = true;
            self.metrics.inc("bus_received");
            let _ = self.local.send(change);
        }
        Ok(())
    }

    /// Sends buffered events, for a clean stop.
    pub async fn flush(&self) {
        if let Some(n) = &self.nats {
            let _ = n.client.flush().await;
        }
    }
}
