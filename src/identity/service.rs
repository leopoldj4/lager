//! An external identity service (`[identity]`) that decides alone who a user
//! is and what it may access. Users cannot be managed through this server.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{AuthError, Identity, IdentityProvider, credential_digest};
use crate::config::{Access, IdentityConfig};

/// Checks credentials with the service and keeps positive answers for a
/// short time. Credentials are cached only as a SHA-256 digest.
pub struct IdentityService {
    cfg: IdentityConfig,
    client: reqwest::Client,
    cache: Mutex<HashMap<[u8; 32], (Instant, Identity)>>,
}

#[derive(serde::Deserialize)]
struct IdentityAnswer {
    user: String,
    #[serde(default)]
    access: BTreeMap<String, Access>,
}

impl IdentityService {
    pub fn new(cfg: &IdentityConfig) -> anyhow::Result<Self> {
        let mut b = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(ca) = &cfg.ca_certificate {
            b = b.add_root_certificate(reqwest::Certificate::from_pem(&std::fs::read(ca)?)?);
        }
        Ok(Self {
            cfg: cfg.clone(),
            client: b.build()?,
            cache: Mutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl IdentityProvider for IdentityService {
    async fn authenticate(
        &self,
        user: &str,
        password: &str,
    ) -> Result<Option<Identity>, AuthError> {
        let key = credential_digest(user, password);
        let ttl = Duration::from_secs(self.cfg.cache_secs);
        if let Some((at, id)) = self.cache.lock().unwrap().get(&key)
            && at.elapsed() < ttl
        {
            return Ok(Some(id.clone()));
        }
        let mut req = self
            .client
            .post(&self.cfg.url)
            .header("content-type", "application/json")
            .body(
                serde_json::to_vec(&serde_json::json!({"username": user, "password": password}))
                    .expect("serializes"),
            );
        if let Some(t) = &self.cfg.token {
            req = req.bearer_auth(t);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AuthError::Unavailable(e.without_url().to_string()))?;
        match resp.status().as_u16() {
            200 => {}
            401 | 403 => return Err(AuthError::BadCredentials),
            s => {
                return Err(AuthError::Unavailable(format!(
                    "identity service status {s}"
                )));
            }
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| AuthError::Unavailable(e.without_url().to_string()))?;
        let answer: IdentityAnswer = serde_json::from_slice(&body)
            .map_err(|e| AuthError::Unavailable(format!("identity service answer: {e}")))?;
        let id = Identity::User {
            name: answer.user,
            access: answer.access,
            token: None,
        };
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|_, (at, _)| at.elapsed() < ttl);
        cache.insert(key, (Instant::now(), id.clone()));
        Ok(Some(id))
    }
}
