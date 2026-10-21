//! Passkeys and one-use, browser-bound ceremonies shared by all servers.
//! Only public challenge options leave the server; ceremony state stays in PostgreSQL.
//! Sign-up (docs/decisions/0023-passkey-sign-up.md) creates an account without a
//! password whose only credential is its first passkey.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use webauthn_rs::prelude::*;

use super::{AuthError, PgUsers, password_stamp, validate_user_name};

type Result<T> = std::result::Result<T, AuthError>;

/// Starts the stored password hash of an account without a password. No
/// [`crate::password::PasswordHash`] parses it, so no password matches; the random
/// rest gives each account its own session stamp.
pub const NO_PASSWORD: &str = "!no-password$";

/// Serializes anonymous ceremony starts and sign-ups between servers.
const ANONYMOUS_LOCK: i64 = 0x7275_7374_6769_7402;

/// The label of the passkey that creates an account.
const SIGN_UP_PASSKEY: &str = "Passkey from sign-up";

#[derive(Debug)]
pub enum PasskeyError {
    /// A malformed request, such as an invalid user name.
    Invalid(String),
    /// The account's state forbids it, such as a taken user name.
    Conflict(String),
    Auth(AuthError),
}

impl From<AuthError> for PasskeyError {
    fn from(e: AuthError) -> Self {
        PasskeyError::Auth(e)
    }
}

/// A verified sign-up passkey, before its account exists.
pub struct SignUp {
    name: String,
    handle: Uuid,
    key: Passkey,
}

impl SignUp {
    pub fn name(&self) -> &str {
        &self.name
    }
}

pub fn webauthn(origin: &str) -> anyhow::Result<Webauthn> {
    let url = Url::parse(origin)?;
    anyhow::ensure!(
        url.origin().ascii_serialization() == origin,
        "web.public_origin must be an origin without a path or trailing slash"
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
    .rp_name("lager")
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
        state: DiscoverableAuthentication,
    },
    SignUp {
        state: PasskeyRegistration,
        name: String,
        handle: Uuid,
    },
}

#[derive(Serialize, Deserialize)]
struct Pending {
    user: Option<String>,
    stamp: Option<[u8; 32]>,
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

// Require a discoverable credential so the authenticator can offer the account
// without receiving a user name or credential allow-list during sign-in.
fn discoverable(options: CreationChallengeResponse) -> Result<serde_json::Value> {
    let mut options = serde_json::to_value(options).map_err(unavailable)?;
    options["publicKey"]["authenticatorSelection"]["residentKey"] = "required".into();
    options["publicKey"]["authenticatorSelection"]["requireResidentKey"] = true.into();
    Ok(options)
}

/// Whether a user name, in any case, is taken.
async fn name_taken(conn: &mut PgConnection, name: &str) -> Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lager_users WHERE lower(name) = lower($1))")
        .bind(name)
        .fetch_one(conn)
        .await
        .map_err(unavailable)
}

impl PgUsers {
    pub async fn passkeys(&self, user: &str) -> Result<Vec<PasskeyInfo>> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT credential_id, name FROM lager_passkeys WHERE user_name = $1 ORDER BY created_at, credential_id")
            .bind(user).fetch_all(&self.pool).await.map_err(unavailable)?;
        Ok(rows
            .into_iter()
            .map(|(id, name)| PasskeyInfo { id, name })
            .collect())
    }

    /// Whether the account has a password, as opposed to passkeys only.
    pub async fn has_password(&self, user: &str) -> Result<bool> {
        let hash: Option<String> =
            sqlx::query_scalar("SELECT password_hash FROM lager_users WHERE name = $1")
                .bind(user)
                .fetch_optional(&self.pool)
                .await
                .map_err(unavailable)?;
        Ok(!hash
            .ok_or(AuthError::BadCredentials)?
            .starts_with(NO_PASSWORD))
    }

    /// An account without a password keeps its last passkey, so it can still sign in.
    pub async fn delete_passkey(
        &self,
        user: &str,
        id: &str,
    ) -> std::result::Result<(), PasskeyError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let (hash, _) = account(&mut tx, user).await?;
        let deleted =
            sqlx::query("DELETE FROM lager_passkeys WHERE user_name = $1 AND credential_id = $2")
                .bind(user)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(unavailable)?;
        if deleted.rows_affected() != 1 {
            return Err(AuthError::BadCredentials.into());
        }
        let left: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lager_passkeys WHERE user_name = $1")
                .bind(user)
                .fetch_one(&mut *tx)
                .await
                .map_err(unavailable)?;
        if left == 0 && hash.starts_with(NO_PASSWORD) {
            return Err(PasskeyError::Conflict(
                "your account has no password; add another passkey before removing this one".into(),
            ));
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }

    /// Registration requires the caller to authenticate the user with a browser session.
    pub async fn start_passkey_registration(
        &self,
        origin: &str,
        user: &str,
        name: &str,
    ) -> Result<(String, serde_json::Value)> {
        let webauthn = webauthn(origin).map_err(unavailable)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let (hash, handle) = account(&mut tx, user).await?;
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT credential FROM lager_passkeys WHERE user_name = $1 ORDER BY credential_id",
        )
        .bind(user)
        .fetch_all(&mut *tx)
        .await
        .map_err(unavailable)?;
        let keys: Vec<Passkey> = rows
            .iter()
            .map(|(data,)| decode(data))
            .collect::<Result<_>>()?;
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
        let options = discoverable(options)?;
        let ceremony = Ceremony::Register {
            state,
            name: name.trim().into(),
        };
        // Bounded per account, including concurrent starts on different servers.
        sqlx::query("DELETE FROM lager_passkey_challenges WHERE expires_at <= now() OR id IN (SELECT id FROM lager_passkey_challenges WHERE user_name = $1 ORDER BY expires_at DESC OFFSET 4)")
            .bind(user).execute(&mut *tx).await.map_err(unavailable)?;
        let id = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let pending = Pending {
            user: Some(user.into()),
            stamp: Some(password_stamp(&hash)),
            origin: origin.into(),
            ceremony,
        };
        sqlx::query(
            "INSERT INTO lager_passkey_challenges (id, user_name, state) VALUES ($1, $2, $3)",
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
    async fn take_passkey(&self, id: &str, origin: &str) -> Result<(Pending, String)> {
        let row: Option<(String, String)> = sqlx::query_as("DELETE FROM lager_passkey_challenges WHERE id = $1 AND expires_at > now() RETURNING state, (expires_at - interval '5 minutes')::text")
            .bind(id).fetch_optional(&self.pool).await.map_err(unavailable)?;
        let (state, started_at) = row.ok_or(AuthError::BadCredentials)?;
        let pending: Pending = decode(&state)?;
        if pending.origin != origin {
            return Err(AuthError::BadCredentials);
        }
        Ok((pending, started_at))
    }

    pub async fn finish_passkey_registration(
        &self,
        origin: &str,
        id: &str,
        user: &str,
        credential: &RegisterPublicKeyCredential,
    ) -> Result<()> {
        let (pending, _) = self.take_passkey(id, origin).await?;
        if pending.user.as_deref() != Some(user) {
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
        if Some(password_stamp(&hash)) != pending.stamp {
            return Err(AuthError::BadCredentials);
        }
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lager_passkeys WHERE user_name = $1")
                .bind(user)
                .fetch_one(&mut *tx)
                .await
                .map_err(unavailable)?;
        if count >= 20 {
            return Err(AuthError::BadCredentials);
        }
        let inserted = sqlx::query("INSERT INTO lager_passkeys (credential_id, user_name, name, credential) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
            .bind(credential_id(&key)).bind(user).bind(name).bind(encode(&key)?).execute(&mut *tx).await.map_err(unavailable)?;
        if inserted.rows_affected() != 1 {
            return Err(AuthError::BadCredentials);
        }
        tx.commit().await.map_err(unavailable)
    }

    /// Start sign-in before an account is known. Only browser-held state is an opaque cookie.
    pub async fn start_passkey_authentication(
        &self,
        origin: &str,
        previous: Option<&str>,
    ) -> Result<(String, serde_json::Value)> {
        let (mut options, state) = webauthn(origin)
            .map_err(unavailable)?
            .start_discoverable_authentication()
            .map_err(unavailable)?;
        // A button-triggered browser dialog, not conditional autofill.
        options.mediation = None;
        let id = self
            .start_anonymous(
                previous,
                Pending {
                    user: None,
                    stamp: None,
                    origin: origin.into(),
                    ceremony: Ceremony::Authenticate { state },
                },
            )
            .await?;
        Ok((id, serde_json::to_value(options).map_err(unavailable)?))
    }

    /// Stores a ceremony without an account, replacing the browser's previous one.
    async fn start_anonymous(&self, previous: Option<&str>, pending: Pending) -> Result<String> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        // Serialize anonymous starts to enforce a hard bound on unauthenticated state.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(ANONYMOUS_LOCK)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        sqlx::query("DELETE FROM lager_passkey_challenges WHERE expires_at <= now() OR (user_name IS NULL AND id = $1)")
            .bind(previous).execute(&mut *tx).await.map_err(unavailable)?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM lager_passkey_challenges WHERE user_name IS NULL",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(unavailable)?;
        if count >= 1024 {
            return Err(AuthError::Unavailable(
                "too many pending anonymous passkey ceremonies".into(),
            ));
        }
        let id = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        sqlx::query("INSERT INTO lager_passkey_challenges (id, state) VALUES ($1, $2)")
            .bind(&id)
            .bind(encode(&pending)?)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(id)
    }

    pub async fn finish_passkey_authentication(
        &self,
        origin: &str,
        id: &str,
        credential: &PublicKeyCredential,
    ) -> Result<(String, [u8; 32])> {
        let (pending, started_at) = self.take_passkey(id, origin).await?;
        let Ceremony::Authenticate { state } = pending.ceremony else {
            return Err(AuthError::BadCredentials);
        };
        let webauthn = webauthn(origin).map_err(unavailable)?;
        // These identifiers are untrusted until the credential's signature is verified.
        let (handle, credential_id) = webauthn
            .identify_discoverable_authentication(credential)
            .map_err(|_| AuthError::BadCredentials)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT u.name, u.password_hash, p.credential
             FROM lager_users u JOIN lager_passkeys p ON p.user_name = u.name
             WHERE u.passkey_user_id = $1 AND p.credential_id = $2
               AND u.updated_at <= $3::text::timestamptz
             FOR UPDATE OF u, p",
        )
        .bind(handle.to_string())
        .bind(URL_SAFE_NO_PAD.encode(credential_id))
        .bind(started_at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        let (user, hash, data) = row.ok_or(AuthError::BadCredentials)?;
        let mut key: Passkey = decode(&data)?;
        let result = webauthn
            .finish_discoverable_authentication(credential, state, &[DiscoverableKey::from(&key)])
            .map_err(|_| AuthError::BadCredentials)?;
        key.update_credential(&result);
        // Verify and update the current credential while holding its row lock, so removal
        // and concurrent counter updates cannot authenticate using stale credential data.
        sqlx::query(
            "UPDATE lager_passkeys SET credential = $1 WHERE credential_id = $2 AND user_name = $3",
        )
        .bind(encode(&key)?)
        .bind(self::credential_id(&key))
        .bind(&user)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok((user, password_stamp(&hash)))
    }
}

impl PgUsers {
    /// Starts creating an account for a free user name. Its passkey user handle is
    /// fixed now, so the authenticator stores the one the account will have.
    pub async fn start_sign_up(
        &self,
        origin: &str,
        name: &str,
        previous: Option<&str>,
    ) -> std::result::Result<(String, serde_json::Value), PasskeyError> {
        validate_user_name(name).map_err(|e| PasskeyError::Invalid(e.to_string()))?;
        if name_taken(&mut *self.pool.acquire().await.map_err(unavailable)?, name).await? {
            return Err(taken(name));
        }
        let handle = Uuid::new_v4();
        let (options, state) = webauthn(origin)
            .map_err(unavailable)?
            .start_passkey_registration(handle, name, name, None)
            .map_err(|_| AuthError::BadCredentials)?;
        let options = discoverable(options)?;
        let id = self
            .start_anonymous(
                previous,
                Pending {
                    user: None,
                    stamp: None,
                    origin: origin.into(),
                    ceremony: Ceremony::SignUp {
                        state,
                        name: name.into(),
                        handle,
                    },
                },
            )
            .await?;
        Ok((id, options))
    }

    /// Verifies the new passkey. The account does not exist until [`Self::create_signed_up`].
    pub async fn finish_sign_up(
        &self,
        origin: &str,
        id: &str,
        credential: &RegisterPublicKeyCredential,
    ) -> Result<SignUp> {
        let (pending, _) = self.take_passkey(id, origin).await?;
        let Ceremony::SignUp {
            state,
            name,
            handle,
        } = pending.ceremony
        else {
            return Err(AuthError::BadCredentials);
        };
        let key = webauthn(origin)
            .map_err(unavailable)?
            .finish_passkey_registration(credential, &state)
            .map_err(|_| AuthError::BadCredentials)?;
        Ok(SignUp { name, handle, key })
    }

    /// Creates the account of a verified sign-up with no access levels, and answers
    /// with its session stamp.
    pub async fn create_signed_up(
        &self,
        sign_up: SignUp,
    ) -> std::result::Result<[u8; 32], PasskeyError> {
        let SignUp { name, handle, key } = sign_up;
        let hash = format!(
            "{NO_PASSWORD}{}",
            URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
        );
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        // Names that differ only in case are taken by whichever sign-up commits first.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(ANONYMOUS_LOCK)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        if name_taken(&mut tx, &name).await? {
            return Err(taken(&name));
        }
        sqlx::query(
            "INSERT INTO lager_users (name, password_hash, updated_by, passkey_user_id)
             VALUES ($1, $2, $1, $3)",
        )
        .bind(&name)
        .bind(&hash)
        .bind(handle.to_string())
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        let inserted = sqlx::query("INSERT INTO lager_passkeys (credential_id, user_name, name, credential) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
            .bind(credential_id(&key)).bind(&name).bind(SIGN_UP_PASSKEY).bind(encode(&key)?).execute(&mut *tx).await.map_err(unavailable)?;
        if inserted.rows_affected() != 1 {
            return Err(PasskeyError::Conflict(
                "this passkey is already registered; try another authenticator".into(),
            ));
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(password_stamp(&hash))
    }
}

fn taken(name: &str) -> PasskeyError {
    PasskeyError::Conflict(format!("the user name {name} is taken"))
}

async fn account(conn: &mut PgConnection, user: &str) -> Result<(String, String)> {
    sqlx::query_as(
        "SELECT password_hash, passkey_user_id FROM lager_users WHERE name = $1 FOR UPDATE",
    )
    .bind(user)
    .fetch_optional(conn)
    .await
    .map_err(unavailable)?
    .ok_or(AuthError::BadCredentials)
}
