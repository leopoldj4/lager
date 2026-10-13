//! Passkeys and one-use, browser-bound ceremonies shared by all servers.
//! Only public challenge options leave the server; ceremony state stays in PostgreSQL.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use webauthn_rs::prelude::*;

use super::{AuthError, PgUsers, TOKEN_PREFIX, password_stamp};

type Result<T> = std::result::Result<T, AuthError>;

pub fn webauthn(origin: &str) -> anyhow::Result<Webauthn> {
    let url = Url::parse(origin)?;
    anyhow::ensure!(
        url.origin().ascii_serialization() == origin,
        "web.passkey_origin must be an origin without a path or trailing slash"
    );
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && url.host_str() == Some("localhost")),
        "passkeys require HTTPS (or http://localhost for development)"
    );
    Ok(WebauthnBuilder::new(
        url.host_str()
            .ok_or_else(|| anyhow::anyhow!("missing passkey host"))?,
        &url,
    )?
    .rp_name("worktree")
    .build()?)
}

#[derive(Serialize)]
pub struct PasskeyInfo {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
enum Ceremony {
    Register {
        state: PasskeyRegistration,
        name: String,
    },
    Authenticate {
        state: PasskeyAuthentication,
        keys: Vec<Passkey>,
    },
}

#[derive(Serialize, Deserialize)]
struct Pending {
    user: String,
    stamp: [u8; 32],
    origin: String,
    ceremony: Ceremony,
}

fn unavailable(e: impl std::fmt::Display) -> AuthError {
    AuthError::Unavailable(e.to_string())
}
fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(unavailable)
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(unavailable)
}
fn credential_id(key: &Passkey) -> String {
    URL_SAFE_NO_PAD.encode(key.cred_id())
}

impl PgUsers {
    pub async fn passkeys(&self, user: &str) -> Result<Vec<PasskeyInfo>> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT credential_id, name FROM worktree_passkeys WHERE user_name = $1 ORDER BY created_at, credential_id")
            .bind(user).fetch_all(&self.pool).await.map_err(unavailable)?;
        Ok(rows
            .into_iter()
            .map(|(id, name)| PasskeyInfo { id, name })
            .collect())
    }

    pub async fn delete_passkey(&self, user: &str, id: &str) -> Result<()> {
        let deleted = sqlx::query(
            "DELETE FROM worktree_passkeys WHERE user_name = $1 AND credential_id = $2",
        )
        .bind(user)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        if deleted.rows_affected() != 1 {
            return Err(AuthError::BadCredentials);
        }
        Ok(())
    }

    /// A password confirmation is required to enroll an additional login method.
    pub async fn start_passkey(
        &self,
        origin: &str,
        user: &str,
        registration: Option<(&str, &str)>,
    ) -> Result<(String, serde_json::Value)> {
        let webauthn = webauthn(origin).map_err(unavailable)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let (hash, handle) = account(&mut tx, user).await?;
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT credential FROM worktree_passkeys WHERE user_name = $1 ORDER BY credential_id",
        )
        .bind(user)
        .fetch_all(&mut *tx)
        .await
        .map_err(unavailable)?;
        let keys: Vec<Passkey> = rows
            .iter()
            .map(|(data,)| decode(data))
            .collect::<Result<_>>()?;
        let (options, ceremony) = if let Some((password, name)) = registration {
            if password.starts_with(TOKEN_PREFIX)
                || !self.verified.verify(user, password, &hash).await
            {
                return Err(AuthError::BadCredentials);
            }
            if keys.len() >= 20 || name.trim().is_empty() || name.len() > 100 {
                return Err(AuthError::BadCredentials);
            }
            let (options, state) = webauthn
                .start_passkey_registration(
                    Uuid::parse_str(&handle).map_err(unavailable)?,
                    user,
                    user,
                    Some(keys.iter().map(|k| k.cred_id().clone()).collect()),
                )
                .map_err(|_| AuthError::BadCredentials)?;
            (
                serde_json::to_value(options).map_err(unavailable)?,
                Ceremony::Register {
                    state,
                    name: name.trim().into(),
                },
            )
        } else {
            let (options, state) = webauthn
                .start_passkey_authentication(&keys)
                .map_err(|_| AuthError::BadCredentials)?;
            (
                serde_json::to_value(options).map_err(unavailable)?,
                Ceremony::Authenticate { state, keys },
            )
        };
        // Bounded per account, including concurrent starts on different servers.
        sqlx::query("DELETE FROM worktree_passkey_challenges WHERE expires_at <= now() OR id IN (SELECT id FROM worktree_passkey_challenges WHERE user_name = $1 ORDER BY expires_at DESC OFFSET 4)")
            .bind(user).execute(&mut *tx).await.map_err(unavailable)?;
        let id = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let pending = Pending {
            user: user.into(),
            stamp: password_stamp(&hash),
            origin: origin.into(),
            ceremony,
        };
        sqlx::query(
            "INSERT INTO worktree_passkey_challenges (id, user_name, state) VALUES ($1, $2, $3)",
        )
        .bind(&id)
        .bind(user)
        .bind(encode(&pending)?)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok((id, options))
    }

    /// Consume before verification, including failed attempts. The id is an HttpOnly cookie.
    async fn take_passkey(&self, id: &str, origin: &str) -> Result<Pending> {
        let row: Option<(String,)> = sqlx::query_as("DELETE FROM worktree_passkey_challenges WHERE id = $1 AND expires_at > now() RETURNING state")
            .bind(id).fetch_optional(&self.pool).await.map_err(unavailable)?;
        let pending: Pending = decode(&row.ok_or(AuthError::BadCredentials)?.0)?;
        if pending.origin != origin {
            return Err(AuthError::BadCredentials);
        }
        Ok(pending)
    }

    pub async fn finish_passkey_registration(
        &self,
        origin: &str,
        id: &str,
        user: &str,
        credential: &RegisterPublicKeyCredential,
    ) -> Result<()> {
        let pending = self.take_passkey(id, origin).await?;
        if pending.user != user {
            return Err(AuthError::BadCredentials);
        }
        let Ceremony::Register { state, name } = pending.ceremony else {
            return Err(AuthError::BadCredentials);
        };
        let key = webauthn(origin)
            .map_err(unavailable)?
            .finish_passkey_registration(credential, &state)
            .map_err(|_| AuthError::BadCredentials)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let (hash, _) = account(&mut tx, user).await?;
        if password_stamp(&hash) != pending.stamp {
            return Err(AuthError::BadCredentials);
        }
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM worktree_passkeys WHERE user_name = $1")
                .bind(user)
                .fetch_one(&mut *tx)
                .await
                .map_err(unavailable)?;
        if count >= 20 {
            return Err(AuthError::BadCredentials);
        }
        let inserted = sqlx::query("INSERT INTO worktree_passkeys (credential_id, user_name, name, credential) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
            .bind(credential_id(&key)).bind(user).bind(name).bind(encode(&key)?).execute(&mut *tx).await.map_err(unavailable)?;
        if inserted.rows_affected() != 1 {
            return Err(AuthError::BadCredentials);
        }
        tx.commit().await.map_err(unavailable)
    }

    pub async fn finish_passkey_authentication(
        &self,
        origin: &str,
        id: &str,
        credential: &PublicKeyCredential,
    ) -> Result<(String, [u8; 32])> {
        let pending = self.take_passkey(id, origin).await?;
        let Ceremony::Authenticate { state, mut keys } = pending.ceremony else {
            return Err(AuthError::BadCredentials);
        };
        let result = webauthn(origin)
            .map_err(unavailable)?
            .finish_passkey_authentication(credential, &state)
            .map_err(|_| AuthError::BadCredentials)?;
        let key = keys
            .iter_mut()
            .find(|k| k.cred_id() == result.cred_id())
            .ok_or(AuthError::BadCredentials)?;
        let old = encode(key)?;
        key.update_credential(&result);
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let (hash, _) = account(&mut tx, &pending.user).await?;
        if password_stamp(&hash) != pending.stamp {
            return Err(AuthError::BadCredentials);
        }
        // A removed or concurrently updated credential cannot authenticate from stale state.
        let updated = sqlx::query("UPDATE worktree_passkeys SET credential = $1 WHERE credential_id = $2 AND user_name = $3 AND credential = $4")
            .bind(encode(key)?).bind(credential_id(key)).bind(&pending.user).bind(old).execute(&mut *tx).await.map_err(unavailable)?;
        if updated.rows_affected() != 1 {
            return Err(AuthError::BadCredentials);
        }
        tx.commit().await.map_err(unavailable)?;
        Ok((pending.user, pending.stamp))
    }
}

async fn account(conn: &mut PgConnection, user: &str) -> Result<(String, String)> {
    sqlx::query_as(
        "SELECT password_hash, passkey_user_id FROM worktree_users WHERE name = $1 FOR UPDATE",
    )
    .bind(user)
    .fetch_optional(conn)
    .await
    .map_err(unavailable)?
    .ok_or(AuthError::BadCredentials)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_requires_a_canonical_secure_origin() {
        assert!(webauthn("https://git.example.com").is_ok());
        assert!(webauthn("http://localhost:3000").is_ok());
        for origin in [
            "http://git.example.com",
            "https://git.example.com/",
            "https://git.example.com/path",
            "https://user@git.example.com",
            "https://git.example.com?x=1",
        ] {
            assert!(webauthn(origin).is_err(), "{origin}");
        }
    }

    /// Run against a disposable database with WORKTREE_TEST_DATABASE_URL set.
    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn passkey_lifecycle() {
        use crate::{
            config::DatabaseConfig,
            identity::{NewUser, UserChange},
            password::PasswordHash,
        };
        use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
        let cfg = DatabaseConfig {
            url: std::env::var("WORKTREE_TEST_DATABASE_URL").expect("test database URL"),
        };
        let users = PgUsers::connect(&cfg).await.unwrap();
        let other_server = PgUsers::connect(&cfg).await.unwrap();
        let user = format!("passkey-{}", Uuid::new_v4());
        let password = "test-password-for-passkeys";
        users
            .create(
                NewUser {
                    name: user.clone(),
                    password_hash: PasswordHash::new(password, 10_000).to_string(),
                    access: Default::default(),
                },
                "test",
            )
            .await
            .unwrap();
        let origin = "https://git.example.com";
        let url = Url::parse(origin).unwrap();
        let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));

        assert!(
            users
                .start_passkey(origin, &user, Some(("wrong", "Laptop")))
                .await
                .is_err()
        );
        assert!(
            users
                .start_passkey(origin, &user, Some(("rgpat_fake", "Laptop")))
                .await
                .is_err()
        );
        assert!(
            users
                .start_passkey(origin, "missing-user", None)
                .await
                .is_err()
        );
        let (id, options) = users
            .start_passkey(origin, &user, Some((password, "Laptop")))
            .await
            .unwrap();
        let credential = authenticator
            .do_registration(url.clone(), serde_json::from_value(options).unwrap())
            .unwrap();
        other_server
            .finish_passkey_registration(origin, &id, &user, &credential)
            .await
            .unwrap();
        assert!(
            users
                .finish_passkey_registration(origin, &id, &user, &credential)
                .await
                .is_err()
        );
        let keys = users.passkeys(&user).await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].name, "Laptop");

        // Finish on another server; replay and missing browser binding fail.
        let (id, options) = users.start_passkey(origin, &user, None).await.unwrap();
        let assertion = authenticator
            .do_authentication(url.clone(), serde_json::from_value(options).unwrap())
            .unwrap();
        assert!(
            users
                .finish_passkey_authentication(origin, "wrong-cookie", &assertion)
                .await
                .is_err()
        );
        let (first, second) = tokio::join!(
            users.finish_passkey_authentication(origin, &id, &assertion),
            other_server.finish_passkey_authentication(origin, &id, &assertion),
        );
        assert_ne!(
            first.is_ok(),
            second.is_ok(),
            "only one server may consume a challenge"
        );
        let (signed_in, stamp) = first.or(second).unwrap();
        assert_eq!(signed_in, user);
        assert_eq!(stamp, users.lookup(&user).await.unwrap().unwrap().stamp);
        assert!(
            users
                .finish_passkey_authentication(origin, &id, &assertion)
                .await
                .is_err()
        );

        // Expiry, wrong origin, tampering, password changes, and revocation each fail.
        for scenario in ["expired", "origin", "tampered", "password", "removed"] {
            let (id, options) = users.start_passkey(origin, &user, None).await.unwrap();
            let mut assertion = authenticator
                .do_authentication(url.clone(), serde_json::from_value(options).unwrap())
                .unwrap();
            match scenario {
                "expired" => {
                    sqlx::query("UPDATE worktree_passkey_challenges SET expires_at = now() - interval '1 second' WHERE id = $1").bind(&id).execute(&users.pool).await.unwrap();
                }
                "tampered" => {
                    assertion.response.signature = vec![0u8; 64].into();
                }
                "password" => {
                    users
                        .update(
                            &user,
                            UserChange {
                                password_hash: Some(
                                    PasswordHash::new("replacement-password", 10_000).to_string(),
                                ),
                                access: None,
                            },
                            "test",
                        )
                        .await
                        .unwrap();
                }
                "removed" => {
                    users.delete_passkey(&user, &keys[0].id).await.unwrap();
                }
                _ => {}
            }
            let request_origin = if scenario == "origin" {
                "https://evil.example.com"
            } else {
                origin
            };
            assert!(
                users
                    .finish_passkey_authentication(request_origin, &id, &assertion)
                    .await
                    .is_err(),
                "{scenario}"
            );
        }
        assert!(users.passkeys(&user).await.unwrap().is_empty());
        users.delete(&user, "test").await.unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM worktree_passkey_challenges WHERE user_name = $1",
        )
        .bind(&user)
        .fetch_one(&users.pool)
        .await
        .unwrap();
        assert_eq!(count, 0);
    }
}
