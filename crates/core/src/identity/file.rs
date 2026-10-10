//! Users in the configuration file: development passwords on the loopback
//! address, salted hashes elsewhere. They change when the file is reloaded.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use super::{AuthError, Identity, IdentityProvider, User, UserError, UserSource, VerifiedHashes};
use crate::config::{Config, UserConfig};
use crate::password::constant_time_eq;

pub struct FileUsers {
    users: RwLock<HashMap<String, UserConfig>>,
    verified: VerifiedHashes,
}

impl FileUsers {
    pub fn new(cfg: &Config) -> Self {
        Self {
            users: RwLock::new(table(cfg)),
            verified: VerifiedHashes::default(),
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.users.read().unwrap().contains_key(name)
    }
}

fn table(cfg: &Config) -> HashMap<String, UserConfig> {
    cfg.users
        .iter()
        .map(|u| (u.name.clone(), u.clone()))
        .collect()
}

#[async_trait]
impl IdentityProvider for FileUsers {
    async fn authenticate(
        &self,
        user: &str,
        password: &str,
    ) -> Result<Option<Identity>, AuthError> {
        let Some(u) = self.users.read().unwrap().get(user).cloned() else {
            return Ok(None);
        };
        let ok = match (&u.password_hash, &u.password_sha256, &u.password) {
            (Some(h), _, _) => self.verified.verify(user, password, h).await,
            (None, Some(h), _) => hex::decode(h)
                .map(|h| constant_time_eq(&Sha256::digest(password.as_bytes()), &h))
                .unwrap_or(false),
            (None, None, Some(p)) => constant_time_eq(
                &Sha256::digest(password.as_bytes()),
                &Sha256::digest(p.as_bytes()),
            ),
            (None, None, None) => false,
        };
        if ok {
            Ok(Some(Identity::User {
                name: u.name,
                access: u.access,
            }))
        } else {
            Err(AuthError::BadCredentials)
        }
    }

    async fn list(&self) -> Result<Vec<User>, UserError> {
        let mut out: Vec<User> = self
            .users
            .read()
            .unwrap()
            .values()
            .map(|u| User {
                name: u.name.clone(),
                access: u.access.clone(),
                source: UserSource::Config,
                created_ms: None,
                updated_ms: None,
                updated_by: None,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn reload(&self, cfg: &Config) {
        *self.users.write().unwrap() = table(cfg);
        self.verified.clear();
    }
}
