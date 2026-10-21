//! The live connection: one WebSocket per browser tab for read queries and
//! change events (docs/decisions/0013-live-connection-protocol.md).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, broadcast, mpsc};

use super::read::{self, ReadError};
use super::{Web, session};
use crate::bus::Event;
use crate::http::AppState;
use lager_core::identity::{AuthError, Identity};
use lager_core::model::RepoId;
use lager_core::repo::Access;

pub const VERSION: u32 = 1;
const HEARTBEAT: Duration = Duration::from_secs(25);
/// A client that sends nothing, not even a pong, for this long is gone.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(70);
const MAX_SUBS: usize = 100;
const MAX_REQUESTS_IN_FLIGHT: usize = 8;
const QUEUE: usize = 256;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Close codes.
const CLOSE_RESTART: u16 = 1012;
const CLOSE_PROTOCOL: u16 = 1008;
const CLOSE_BEHIND: u16 = 1013;
/// The session ended; sign in again.
const CLOSE_SIGNED_OUT: u16 = 4401;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Hello {
        v: u32,
    },
    Sub {
        #[serde(default)]
        id: Option<u64>,
        repo: String,
    },
    Unsub {
        #[serde(default)]
        id: Option<u64>,
        repo: String,
    },
    Req {
        id: u64,
        #[serde(flatten)]
        query: read::Query,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    Welcome {
        v: u32,
        server: String,
        user: Option<String>,
    },
    Subbed {
        id: Option<u64>,
        repo: String,
        generation: u64,
    },
    Unsubbed {
        id: Option<u64>,
        repo: String,
    },
    Changed {
        repo: String,
        generation: u64,
    },
    /// A pull request changed (docs/decisions/0019-pull-requests.md).
    Pull {
        repo: String,
        number: u64,
        version: u64,
    },
    Res {
        id: u64,
        data: serde_json::Value,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        code: &'static str,
        message: String,
    },
    Resync {
        #[serde(skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
    },
    Heartbeat,
}

/// `GET /-/api/live`. Signed-in users connect with their session cookie;
/// without one, the connection sees repositories with anonymous read.
pub async fn upgrade(
    State(s): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(web) = s.web.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !web.origin_allowed(&headers) {
        s.registry.svc.metrics.inc("web_origin_rejections");
        return (StatusCode::FORBIDDEN, "origin not allowed\n").into_response();
    }
    let cookie = session::from_headers(&headers);
    let identity = match web.identity(&s.directory, &headers).await {
        Ok(i) => i,
        Err(r) => return r,
    };
    // An invalid cookie is no session; the client sees `user: null`.
    let cookie = cookie.filter(|_| identity != Identity::Anonymous);
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            web.connections.fetch_add(1, Ordering::Relaxed);
            s.registry.svc.metrics.inc("live_connections");
            Connection::new(s, web.clone(), cookie, identity)
                .run(socket)
                .await;
            web.connections.fetch_sub(1, Ordering::Relaxed);
        })
}

struct Sub {
    name: String,
    /// Events at or below this generation are not sent.
    generation: u64,
}

struct Connection {
    s: AppState,
    web: Arc<Web>,
    cookie: Option<String>,
    identity: Arc<Identity>,
    subs: HashMap<RepoId, Sub>,
    out: mpsc::Sender<ServerMsg>,
    out_rx: Option<mpsc::Receiver<ServerMsg>>,
    requests: Arc<Semaphore>,
    /// Events were dropped for a full queue; a `resync` is owed.
    owe_resync: bool,
}

enum End {
    Close(u16, &'static str),
    Gone,
}

impl Connection {
    fn new(s: AppState, web: Arc<Web>, cookie: Option<String>, identity: Identity) -> Self {
        let (out, out_rx) = mpsc::channel(QUEUE);
        Self {
            s,
            web,
            cookie,
            identity: Arc::new(identity),
            subs: HashMap::new(),
            out,
            out_rx: Some(out_rx),
            requests: Arc::new(Semaphore::new(MAX_REQUESTS_IN_FLIGHT)),
            owe_resync: false,
        }
    }

    async fn run(mut self, socket: WebSocket) {
        let (mut sink, mut stream) = socket.split();
        let mut out_rx = self.out_rx.take().expect("set");
        // Subscribed before any `sub` reads a head (decision 13, order on sub).
        let mut changes = self.s.registry.svc.bus.subscribe();
        let mut stop = self.web.stop.clone();
        let (ping_tx, mut ping_rx) = mpsc::channel::<()>(1);
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
        let text = |m: ServerMsg| {
            Message::Text(Utf8Bytes::from(
                serde_json::to_string(&m).expect("serializes"),
            ))
        };
        let writer = tokio::spawn(async move {
            loop {
                let msg = tokio::select! {
                    m = out_rx.recv() => match m {
                        Some(m) => text(m),
                        None => break,
                    },
                    Some(()) = ping_rx.recv() => Message::Ping(Default::default()),
                    // Send what is queued, but do not wait for running queries.
                    _ = &mut done_rx => {
                        while let Ok(m) = out_rx.try_recv() {
                            if sink.send(text(m)).await.is_err() {
                                return None;
                            }
                        }
                        break;
                    }
                };
                if sink.send(msg).await.is_err() {
                    return None;
                }
            }
            Some(sink)
        });
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
        let mut last_heard = Instant::now();
        let end = loop {
            tokio::select! {
                m = stream.next() => {
                    let Some(Ok(m)) = m else { break End::Gone };
                    last_heard = Instant::now();
                    let text = match m {
                        Message::Text(t) => t,
                        Message::Close(_) => break End::Gone,
                        Message::Binary(_) => break End::Close(CLOSE_PROTOCOL, "text messages only"),
                        _ => continue,
                    };
                    match serde_json::from_str::<ClientMsg>(&text) {
                        Ok(m) => {
                            if let Err(e) = self.handle(m).await {
                                break e;
                            }
                        }
                        Err(e) => {
                            self.send(ServerMsg::Error {
                                id: None,
                                repo: None,
                                code: "bad_request",
                                message: format!("invalid message: {e}"),
                            }).await;
                        }
                    }
                }
                c = changes.recv() => match c {
                    Ok(c) => {
                        if let Err(e) = self.deliver(c) {
                            break e;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        self.s.registry.svc.metrics.inc("live_resyncs");
                        self.owe_resync = true;
                        self.pay_resync();
                    }
                    Err(broadcast::error::RecvError::Closed) => break End::Close(CLOSE_RESTART, "server stopping"),
                },
                _ = tick.tick() => {
                    if last_heard.elapsed() > CLIENT_TIMEOUT {
                        break End::Gone;
                    }
                    if self.owe_resync && !self.pay_resync() {
                        break End::Close(CLOSE_BEHIND, "client too slow");
                    }
                    let _ = ping_tx.try_send(());
                    let _ = self.out.try_send(ServerMsg::Heartbeat);
                    if let Err(e) = self.recheck().await {
                        break e;
                    }
                }
                _ = crate::server::stopped(&mut stop) => break End::Close(CLOSE_RESTART, "server stopping"),
            }
        };
        let _ = done_tx.send(());
        drop(self);
        if let Ok(Some(mut sink)) = writer.await
            && let End::Close(code, reason) = end
        {
            let _ = sink
                .send(Message::Close(Some(CloseFrame {
                    code,
                    reason: reason.into(),
                })))
                .await;
        }
    }

    async fn send(&self, m: ServerMsg) {
        let _ = self.out.send(m).await;
    }

    /// Sends the owed `resync`. False when the queue is still full.
    fn pay_resync(&mut self) -> bool {
        if self.out.try_send(ServerMsg::Resync { repo: None }).is_ok() {
            self.owe_resync = false;
        }
        !self.owe_resync
    }

    async fn handle(&mut self, m: ClientMsg) -> Result<(), End> {
        match m {
            ClientMsg::Hello { v } => {
                if v != VERSION {
                    return Err(End::Close(CLOSE_PROTOCOL, "unsupported protocol version"));
                }
                self.send(ServerMsg::Welcome {
                    v: VERSION,
                    server: self.s.registry.svc.bus.server_id().to_string(),
                    user: match &*self.identity {
                        Identity::Anonymous => None,
                        i => Some(i.name().to_string()),
                    },
                })
                .await;
            }
            ClientMsg::Sub { id, repo } => self.subscribe(id, repo).await,
            ClientMsg::Unsub { id, repo } => {
                self.subs.retain(|_, s| s.name != repo);
                self.send(ServerMsg::Unsubbed { id, repo }).await;
            }
            ClientMsg::Req { id, query } => {
                self.s.registry.svc.metrics.inc("live_requests");
                let Ok(permit) = self.requests.clone().try_acquire_owned() else {
                    self.send(ServerMsg::Error {
                        id: Some(id),
                        repo: None,
                        code: "busy",
                        message: "too many requests in flight".into(),
                    })
                    .await;
                    return Ok(());
                };
                let (s, who, out) = (self.s.clone(), self.identity.clone(), self.out.clone());
                tokio::spawn(async move {
                    let msg = match read::run(&s, &who, query).await {
                        Ok(data) => ServerMsg::Res { id, data },
                        Err(e) => ServerMsg::Error {
                            id: Some(id),
                            repo: None,
                            code: e.code,
                            message: e.message,
                        },
                    };
                    let _ = out.send(msg).await;
                    drop(permit);
                });
            }
        }
        Ok(())
    }

    /// Watches a repository: the bus receiver exists already, so reading the
    /// head afterwards loses no push in between.
    async fn subscribe(&mut self, id: Option<u64>, name: String) {
        let error = |code: &'static str, message: String| ServerMsg::Error {
            id,
            repo: Some(name.clone()),
            code,
            message,
        };
        let repo = match read::readable(&self.s, &self.identity, &name).await {
            Ok(r) => r,
            Err(e) => return self.send(error(e.code, e.message)).await,
        };
        if !self.subs.contains_key(&repo.id) && self.subs.len() >= MAX_SUBS {
            let msg = error("too_many_subs", format!("at most {MAX_SUBS} subscriptions"));
            return self.send(msg).await;
        }
        let generation = match self.s.registry.svc.storage.load_head(repo.id).await {
            Ok(Some((head, _))) => head.generation,
            Ok(None) => {
                return self
                    .send(error("not_found", "repository not found".into()))
                    .await;
            }
            Err(e) => {
                let ReadError { code, message } =
                    ReadError::from(crate::repository::RepoError::from(e));
                return self.send(error(code, message)).await;
            }
        };
        self.subs.retain(|_, s| s.name != name);
        self.subs.insert(
            repo.id,
            Sub {
                name: name.clone(),
                generation,
            },
        );
        self.send(ServerMsg::Subbed {
            id,
            repo: name,
            generation,
        })
        .await;
    }

    fn deliver(&mut self, e: Event) -> Result<(), End> {
        let msg = match e {
            Event::Changed(c) => {
                let Some(sub) = self.subs.get_mut(&c.repo) else {
                    return Ok(());
                };
                if c.generation <= sub.generation {
                    return Ok(());
                }
                sub.generation = c.generation;
                ServerMsg::Changed {
                    repo: sub.name.clone(),
                    generation: c.generation,
                }
            }
            // Versions are per pull request; the client drops old ones.
            Event::Pull(p) => {
                let Some(sub) = self.subs.get(&p.repo) else {
                    return Ok(());
                };
                ServerMsg::Pull {
                    repo: sub.name.clone(),
                    number: p.number,
                    version: p.version,
                }
            }
        };
        if self.out.try_send(msg).is_err() {
            self.s.registry.svc.metrics.inc("live_resyncs");
            self.owe_resync = true;
        }
        Ok(())
    }

    /// Looks the session user up again and checks every subscription with
    /// the current access and repository settings.
    async fn recheck(&mut self) -> Result<(), End> {
        if let Some(cookie) = &self.cookie {
            match self.web.sessions.resolve(&self.s.directory, cookie).await {
                Ok(Some(i)) => self.identity = Arc::new(i),
                Ok(None) | Err(AuthError::BadCredentials) => {
                    return Err(End::Close(CLOSE_SIGNED_OUT, "signed out"));
                }
                // Keep the last answer while the user store is unavailable.
                Err(AuthError::Unavailable(_)) => {}
            }
        }
        let mut lost = vec![];
        for (id, sub) in &self.subs {
            match self.s.directory.repo(&sub.name).await {
                Ok(Some(r))
                    if r.id == *id
                        && self.s.directory.access(&self.identity, &r) >= Access::Read => {}
                Ok(_) => lost.push(*id),
                Err(_) => {}
            }
        }
        for id in lost {
            if let Some(sub) = self.subs.remove(&id) {
                self.send(ServerMsg::Error {
                    id: None,
                    repo: Some(sub.name),
                    code: "not_found",
                    message: "repository not found".into(),
                })
                .await;
            }
        }
        Ok(())
    }
}
