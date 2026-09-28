//! Server selection and notification.
//!
//! Rendezvous hashing ranks servers per repository. Routing only reduces
//! duplicate cache work: correctness never depends on it, because every
//! request checks the storage head and CAS decides every write.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures::TryStreamExt;
use sha2::{Digest, Sha256};

use crate::config::RoutingConfig;
use crate::http::AppState;
use crate::model::RepoId;

pub const FORWARDED_HEADER: &str = "x-rustgit-forwarded";
const PEER_TOKEN_HEADER: &str = "x-rustgit-peer-token";
const DOWN_FOR: Duration = Duration::from_secs(10);

pub struct Routing {
    self_id: String,
    cfg: RoutingConfig,
    client: reqwest::Client,
    down: Mutex<HashMap<String, Instant>>,
}

/// Ranks server IDs for a repository, best first.
pub fn rank<'a>(repo: RepoId, servers: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut scored: Vec<([u8; 32], &str)> = servers
        .into_iter()
        .map(|s| {
            let mut h = Sha256::new();
            h.update(repo.0.as_bytes());
            h.update(s.as_bytes());
            (h.finalize().into(), s)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    scored.into_iter().map(|(_, s)| s).collect()
}

impl Routing {
    pub fn new(self_id: String, cfg: RoutingConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            self_id,
            cfg,
            client,
            down: Mutex::new(HashMap::new()),
        })
    }

    fn is_down(&self, id: &str) -> bool {
        let mut down = self.down.lock().unwrap();
        match down.get(id) {
            Some(t) if t.elapsed() < DOWN_FOR => true,
            Some(_) => {
                down.remove(id);
                false
            }
            None => false,
        }
    }

    fn mark_down(&self, id: &str) {
        self.down
            .lock()
            .unwrap()
            .insert(id.to_string(), Instant::now());
    }

    /// The first available server for the repository.
    pub fn preferred(&self, repo: RepoId) -> Option<&crate::config::PeerConfig> {
        let ids = rank(repo, self.cfg.servers.iter().map(|s| s.id.as_str()));
        let id = ids
            .into_iter()
            .find(|id| *id == self.self_id || !self.is_down(id))?;
        self.cfg.servers.iter().find(|s| s.id == id)
    }

    /// Whether this server is the first available server for the repository.
    pub fn is_preferred_self(&self, repo: RepoId) -> bool {
        self.preferred(repo).is_none_or(|p| p.id == self.self_id)
    }

    /// Tells other servers about an accepted push. Failures are ignored.
    pub fn notify(&self, repo: RepoId, generation: u64) {
        if !self.cfg.notify {
            return;
        }
        for peer in self.cfg.servers.iter().filter(|p| p.id != self.self_id) {
            let url = format!("{}/_rustgit/notify", peer.url.trim_end_matches('/'));
            let body = serde_json::to_vec(&Notification { repo, generation }).expect("serializes");
            let mut req = self
                .client
                .post(url)
                .header("content-type", "application/json")
                .body(body)
                .timeout(Duration::from_secs(2));
            if let Some(t) = &self.cfg.peer_token {
                req = req.header(PEER_TOKEN_HEADER, t);
            }
            tokio::spawn(async move {
                let _ = req.send().await;
            });
        }
    }
}

/// Forwards the request to the preferred server, or returns the body for
/// local handling. A request is forwarded at most once, which prevents loops.
pub async fn forward_or_keep(
    s: &AppState,
    method: Method,
    uri: &Uri,
    headers: &HeaderMap,
    repo: RepoId,
    body: Body,
) -> Result<Body, Response> {
    let Some(r) = &s.routing else { return Ok(body) };
    if !r.cfg.forward || headers.contains_key(FORWARDED_HEADER) {
        return Ok(body);
    }
    let Some(peer) = r.preferred(repo) else {
        return Ok(body);
    };
    if peer.id == r.self_id {
        return Ok(body);
    }
    let url = format!(
        "{}{}",
        peer.url.trim_end_matches('/'),
        uri.path_and_query().map(|p| p.as_str()).unwrap_or("/")
    );
    let mut req = r
        .client
        .request(method.clone(), url)
        .header(FORWARDED_HEADER, &r.self_id);
    for name in [
        "authorization",
        "content-type",
        "content-encoding",
        "content-length",
        "git-protocol",
        "accept",
        "user-agent",
    ] {
        if let Some(v) = headers.get(name) {
            req = req.header(name, v);
        }
    }
    s.registry.svc.metrics.inc("forwarded_requests");
    let result = if method == Method::GET {
        req.send().await
    } else {
        req.body(reqwest::Body::wrap_stream(body.into_data_stream()))
            .send()
            .await
    };
    match result {
        Ok(resp) => {
            if resp.headers().contains_key(crate::http::DRAINING_HEADER) {
                // The peer stops soon: send it no further requests.
                r.mark_down(&peer.id);
            }
            let mut out = Response::builder().status(resp.status().as_u16());
            for (k, v) in resp.headers() {
                if is_end_to_end(k) {
                    out = out.header(k.as_str(), v.as_bytes());
                }
            }
            let stream = resp.bytes_stream().map_err(std::io::Error::other);
            Err(out
                .body(Body::from_stream(stream))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()))
        }
        Err(e) => {
            tracing::warn!(peer = %peer.id, error = %e, "forwarding failed; marking server unavailable");
            r.mark_down(&peer.id);
            if method == Method::GET {
                // No request body was consumed: handle locally.
                Ok(Body::empty())
            } else {
                Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "server unavailable; retry\n",
                )
                    .into_response())
            }
        }
    }
}

fn is_end_to_end(h: &HeaderName) -> bool {
    !matches!(
        h.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "transfer-encoding"
            | "upgrade"
            | "te"
            | "trailer"
            | "content-length"
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Notification {
    pub repo: RepoId,
    pub generation: u64,
}

/// Receives a change notification and updates the local cache in the
/// background. Reads still check the storage head themselves.
pub async fn notify_handler(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(r) = &s.routing else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let token = headers.get(PEER_TOKEN_HEADER).and_then(|v| v.to_str().ok());
    if r.cfg.peer_token.as_deref() != token {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(n) = serde_json::from_slice::<Notification>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    s.registry.svc.metrics.inc("notifications_received");
    let controller = s.registry.controller(n.repo);
    tokio::spawn(async move {
        let _ = controller.sync_to_storage().await;
    });
    StatusCode::ACCEPTED.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendezvous_ranking_is_stable_and_moves_few_repositories() {
        let servers = ["a", "b", "c"];
        let repos: Vec<RepoId> = (0..300).map(|_| RepoId::new()).collect();
        let before: Vec<&str> = repos.iter().map(|r| rank(*r, servers)[0]).collect();
        // Same input, same order.
        assert_eq!(
            before,
            repos
                .iter()
                .map(|r| rank(*r, servers)[0])
                .collect::<Vec<_>>()
        );
        // Every server gets some repositories.
        for s in servers {
            assert!(before.iter().filter(|x| **x == s).count() > 50, "{s}");
        }
        // Removing server c moves only the repositories that c had.
        let after: Vec<&str> = repos.iter().map(|r| rank(*r, ["a", "b"])[0]).collect();
        for (b, a) in before.iter().zip(&after) {
            if *b != "c" {
                assert_eq!(b, a);
            }
        }
    }
}
