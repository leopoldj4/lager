//! Webhooks of GitHub mirrors (docs/decisions/0027-live-github-mirrors.md).
//!
//! A sync adds a webhook to the GitHub repository if the mirror has none and
//! the site has a public `https` origin. GitHub permits this only to admins
//! of the repository; a refusal is tried again a day later. The webhook
//! posts to `/-/api/github/hook/<repository UUID>`, signed with a secret of
//! its own; a delivery only pokes the mirror, so the sync reads GitHub as
//! always and a forged or repeated delivery costs one sync at most.

use reqwest::StatusCode;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{API, Github};
use lager_core::github::{Due, Hook};
use lager_core::limits;
use lager_core::model::RepoId;
use lager_core::password::constant_time_eq;

/// What GitHub sends: pushes, branches and tags, pull requests, comments.
const EVENTS: &[&str] = &["push", "create", "delete", "pull_request", "issue_comment"];

#[derive(Deserialize)]
struct ApiHook {
    id: i64,
    config: ApiHookConfig,
}

#[derive(Deserialize)]
struct ApiHookConfig {
    url: Option<String>,
}

/// The answer to a webhook delivery.
pub enum Delivery {
    /// The signature matched; a sync follows.
    Accepted,
    /// GitHub's test after adding the webhook.
    Pong,
    /// No mirror with a webhook here.
    Unknown,
    BadSignature,
}

impl Github {
    fn hook_url(&self, repo: RepoId) -> Option<String> {
        self.public_origin
            .as_ref()
            .map(|o| format!("{o}/-/api/github/hook/{repo}"))
    }

    /// Adds the mirror's webhook. Failures are logged, not the mirror's
    /// error: polling still works.
    pub(super) async fn add_hook(&self, due: &Due, token: &str) {
        let Some(url) = self.hook_url(due.repo) else {
            return;
        };
        let secret = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut res = self.create_hook(token, &due.github, &url, &secret).await;
        // A webhook from an earlier attempt whose answer was lost: its
        // secret is unknown, so it is replaced.
        if matches!(res, Ok(Err(StatusCode::UNPROCESSABLE_ENTITY))) {
            self.delete_hooks_to(token, &due.github, &url).await;
            res = self.create_hook(token, &due.github, &url, &secret).await;
        }
        let recorded = match res {
            Ok(Ok(id)) => {
                tracing::info!(repo = %due.name, github = %due.github, "webhook added");
                self.db.set_hook(due.repo, Some((id, &secret))).await
            }
            // Only admins of the repository may add webhooks.
            Ok(Err(status)) => {
                tracing::info!(repo = %due.name, github = %due.github, %status, "webhook not added; polling only");
                self.db.set_hook(due.repo, None).await
            }
            Err(e) => {
                tracing::info!(repo = %due.name, error = %e, "webhook not added");
                return;
            }
        };
        if let Err(e) = recorded {
            tracing::warn!(repo = %due.name, error = %e, "webhook not recorded");
        }
    }

    /// `Ok(Err(status))`: GitHub refused.
    async fn create_hook(
        &self,
        token: &str,
        github: &str,
        url: &str,
        secret: &str,
    ) -> Result<Result<i64, StatusCode>, reqwest::Error> {
        let res = self
            .request(
                reqwest::Method::POST,
                token,
                &format!("/repos/{github}/hooks"),
            )
            .json(&serde_json::json!({
                "name": "web",
                "active": true,
                "events": EVENTS,
                "config": {
                    "url": url,
                    "content_type": "json",
                    "secret": secret,
                    "insecure_ssl": "0",
                },
            }))
            .send()
            .await?;
        if !res.status().is_success() {
            return Ok(Err(res.status()));
        }
        Ok(Ok(res.json::<ApiHook>().await?.id))
    }

    async fn delete_hooks_to(&self, token: &str, github: &str, url: &str) {
        let path = format!("/repos/{github}/hooks?per_page=100");
        let Ok(hooks) = self.api::<Vec<ApiHook>>(token, &path).await else {
            return;
        };
        for h in hooks
            .iter()
            .filter(|h| h.config.url.as_deref() == Some(url))
        {
            let path = format!("/repos/{github}/hooks/{}", h.id);
            let _ = self
                .request(reqwest::Method::DELETE, token, &path)
                .send()
                .await;
        }
    }

    /// Removes a mirror's webhook from GitHub, if the token still can.
    pub(super) async fn delete_hook(&self, hook: &Hook) {
        let Some(token) = &hook.token else { return };
        let path = format!("/repos/{}/hooks/{}", hook.github, hook.id);
        match self
            .request(reqwest::Method::DELETE, token, &path)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() || r.status() == StatusCode::NOT_FOUND => {
                tracing::info!(github = %hook.github, "webhook removed");
            }
            Ok(r) => {
                tracing::info!(github = %hook.github, status = %r.status(), "webhook not removed")
            }
            Err(e) => tracing::info!(github = %hook.github, error = %e, "webhook not removed"),
        }
    }

    fn request(&self, method: reqwest::Method, token: &str, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{API}{path}"))
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
    }

    /// Checks a delivery's `X-Hub-Signature-256` and pokes its mirror.
    pub async fn receive(
        &self,
        repo: RepoId,
        event: &str,
        signature: &str,
        body: &[u8],
    ) -> Result<Delivery, String> {
        let Some(secret) = self.db.hook_secret(repo).await.map_err(|e| e.to_string())? else {
            return Ok(Delivery::Unknown);
        };
        let expected = format!(
            "sha256={}",
            hex::encode(hmac_sha256(secret.as_bytes(), body))
        );
        if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
            return Ok(Delivery::BadSignature);
        }
        if event == "ping" {
            return Ok(Delivery::Pong);
        }
        if self
            .db
            .poke(repo, limits::GITHUB_SYNC_INTERVAL)
            .await
            .map_err(|e| e.to_string())?
        {
            self.wake.notify_one();
        }
        Ok(Delivery::Accepted)
    }
}

/// HMAC-SHA256 (RFC 2104), as GitHub signs deliveries.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let inner = Sha256::new()
        .chain_update(k.map(|b| b ^ 0x36))
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(k.map(|b| b ^ 0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}
