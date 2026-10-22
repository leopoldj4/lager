//! Users in PostgreSQL (`[database]`), managed with `lager user` and
//! `lager token`. They are the only users.
//!
//! `lager_users` holds one row per user with a salted password hash;
//! `lager_user_access` holds its access levels. Every lookup reads the
//! database, so a change applies to the next request on every server.
//! `lager_tokens` holds personal access tokens as SHA-256 digests, and
//! `lager_token_access` the levels a token is limited to.
//! `lager_ssh_keys` holds public keys for Git over SSH.
//! The schema is brought up to date when the server starts.

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, PgConnection};

use super::{
    AuthError, Identity, NamedUser, NewToken, NewUser, PublicKeyLine, SshKey, SshKeyId,
    TOKEN_PREFIX, Token, TokenId, User, UserChange, UserError, VerifiedHashes, password_stamp,
};
use crate::config::DatabaseConfig;
use crate::limits::{DATABASE_CONNECTIONS, DATABASE_TIMEOUT};
use crate::repo::Access;

/// Schema changes, applied in order and recorded in `lager_schema`.
/// Never edit an applied entry; append a new one.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE lager_users (
    name text PRIMARY KEY,
    password_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text NOT NULL
);
CREATE TABLE lager_user_access (
    user_name text NOT NULL REFERENCES lager_users (name) ON DELETE CASCADE,
    -- A repository name or '*'.
    repository text NOT NULL,
    level text NOT NULL CHECK (level IN ('none', 'read', 'write', 'force')),
    PRIMARY KEY (user_name, repository)
);
CREATE INDEX lager_user_access_repository ON lager_user_access (repository);
CREATE TABLE lager_tokens (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_name text NOT NULL REFERENCES lager_users (name) ON DELETE CASCADE,
    name text NOT NULL,
    -- SHA-256 of the whole token. The token itself is never stored.
    token_sha256 bytea NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    last_used_at timestamptz
);
CREATE INDEX lager_tokens_user ON lager_tokens (user_name);
-- Without rows here a token has its user's access; with rows, at most these
-- levels and never more than the user has.
CREATE TABLE lager_token_access (
    token_id bigint NOT NULL REFERENCES lager_tokens (id) ON DELETE CASCADE,
    repository text NOT NULL,
    level text NOT NULL CHECK (level IN ('none', 'read', 'write', 'force')),
    PRIMARY KEY (token_id, repository)
);
"#,
    r#"
ALTER TABLE lager_users ADD COLUMN passkey_user_id text NOT NULL DEFAULT gen_random_uuid()::text;
CREATE TABLE lager_passkeys (
    credential_id text PRIMARY KEY,
    user_name text NOT NULL REFERENCES lager_users(name) ON DELETE CASCADE,
    name text NOT NULL,
    credential text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX lager_passkeys_user ON lager_passkeys(user_name);
CREATE TABLE lager_passkey_challenges (
    id text PRIMARY KEY,
    user_name text NOT NULL REFERENCES lager_users(name) ON DELETE CASCADE,
    state text NOT NULL,
    expires_at timestamptz NOT NULL DEFAULT now() + interval '5 minutes'
);
CREATE INDEX lager_passkey_challenges_user ON lager_passkey_challenges(user_name);
CREATE INDEX lager_passkey_challenges_expiry ON lager_passkey_challenges(expires_at);
"#,
    r#"
ALTER TABLE lager_passkey_challenges ALTER COLUMN user_name DROP NOT NULL;
-- Pending login state changes shape for account discovery.
DELETE FROM lager_passkey_challenges;
"#,
    // Pull requests (docs/decisions/0019-pull-requests.md), by repository
    // UUID: a reused name starts without the old pull requests.
    r#"
CREATE TABLE lager_pull_numbers (
    repository text PRIMARY KEY,
    last bigint NOT NULL
);
CREATE TABLE lager_pulls (
    repository text NOT NULL,
    number bigint NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    author text NOT NULL,
    -- Branch short names.
    source text NOT NULL,
    target text NOT NULL,
    state text NOT NULL CHECK (state IN ('open', 'closed', 'merged')),
    -- The source and target commits the server last recorded, and their
    -- merge base: the diff of a pull request starts there. After a merge,
    -- the target is the merge commit.
    head_oid text NOT NULL,
    target_oid text NOT NULL,
    merge_base text NOT NULL,
    merge_oid text,
    -- NULL for a merge by a push.
    merged_by text,
    -- The client's request UUIDs of the creation and the merge, for retries.
    request text UNIQUE,
    merge_request text,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    closed_at timestamptz,
    PRIMARY KEY (repository, number)
);
CREATE INDEX lager_pulls_state ON lager_pulls (repository, state, number);
CREATE UNIQUE INDEX lager_pulls_open_branches
    ON lager_pulls (repository, source, target) WHERE state = 'open';
CREATE TABLE lager_pull_events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repository text NOT NULL,
    number bigint NOT NULL,
    kind text NOT NULL CHECK (kind IN ('comment', 'push', 'closed', 'reopened', 'merged')),
    -- NULL for the server.
    author text,
    body text,
    old_oid text,
    oid text,
    request text UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (repository, number) REFERENCES lager_pulls (repository, number)
        ON DELETE CASCADE
);
CREATE INDEX lager_pull_events_pull ON lager_pull_events (repository, number, id);
"#,
    // GitHub accounts and read-only mirrors
    // (docs/decisions/0020-github-mirrors.md).
    r#"
CREATE TABLE lager_github_accounts (
    user_name text PRIMARY KEY REFERENCES lager_users (name) ON DELETE CASCADE,
    github_id bigint NOT NULL,
    login text NOT NULL,
    -- The OAuth token. Git needs it in plain text to fetch.
    token text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE lager_github_mirrors (
    -- The repository UUID.
    repository text PRIMARY KEY,
    name text NOT NULL,
    -- Whose GitHub account the sync uses.
    user_name text NOT NULL REFERENCES lager_users (name) ON DELETE CASCADE,
    -- owner/name on GitHub.
    github text NOT NULL,
    -- A server that starts a sync moves this ahead by a lease, so others
    -- skip the mirror meanwhile.
    next_sync_at timestamptz NOT NULL DEFAULT now(),
    synced_at timestamptz,
    error text,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX lager_github_mirrors_due ON lager_github_mirrors (next_sync_at);
CREATE INDEX lager_github_mirrors_user ON lager_github_mirrors (user_name);
"#,
    // Git over SSH (docs/decisions/0022-git-over-ssh.md). A key signs in
    // as one user, so a fingerprint is unique over all users.
    r#"
CREATE TABLE lager_ssh_keys (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_name text NOT NULL REFERENCES lager_users (name) ON DELETE CASCADE,
    name text NOT NULL,
    -- 'SHA256:' and the unpadded base64 digest of the wire encoding.
    fingerprint text NOT NULL UNIQUE,
    -- '<type> <base64>', without the comment.
    public_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz
);
CREATE INDEX lager_ssh_keys_user ON lager_ssh_keys (user_name);
"#,
    // Pull requests of GitHub mirrors (docs/decisions/0026-github-pull-requests.md).
    // A pull request from GitHub has its page there; its authors are
    // GitHub logins, and GitHub decides which ones are open.
    r#"
ALTER TABLE lager_pulls ADD COLUMN github_url text;
DROP INDEX lager_pulls_open_branches;
CREATE UNIQUE INDEX lager_pulls_open_branches
    ON lager_pulls (repository, source, target) WHERE state = 'open' AND github_url IS NULL;
-- GitHub's comment ID, unique per repository: two users may mirror the
-- same GitHub repository.
ALTER TABLE lager_pull_events ADD COLUMN github_id bigint;
ALTER TABLE lager_pull_events ADD COLUMN edited_at timestamptz;
CREATE UNIQUE INDEX lager_pull_events_github
    ON lager_pull_events (repository, github_id) WHERE github_id IS NOT NULL;
CREATE INDEX lager_pull_events_time ON lager_pull_events (repository, number, created_at, id);
-- Where the import stands: GitHub's `updated_at` of the newest pull
-- request and comment seen, the next page of the first import of pull
-- requests (NULL once it is done), and the ETags of the last answers.
ALTER TABLE lager_github_mirrors ADD COLUMN pulls_since text;
ALTER TABLE lager_github_mirrors ADD COLUMN pulls_etag text;
ALTER TABLE lager_github_mirrors ADD COLUMN pulls_import_page integer DEFAULT 1;
ALTER TABLE lager_github_mirrors ADD COLUMN comments_since text;
ALTER TABLE lager_github_mirrors ADD COLUMN comments_page integer NOT NULL DEFAULT 1;
ALTER TABLE lager_github_mirrors ADD COLUMN comments_etag text;
"#,
    // Live mirrors (docs/decisions/0027-live-github-mirrors.md): until when a
    // page watches the mirror, when a sync last started and when GitHub or a
    // page last asked for one, and the mirror's webhook on GitHub.
    r#"
ALTER TABLE lager_github_mirrors ADD COLUMN watched_until timestamptz;
ALTER TABLE lager_github_mirrors ADD COLUMN claimed_at timestamptz;
ALTER TABLE lager_github_mirrors ADD COLUMN poked_at timestamptz;
ALTER TABLE lager_github_mirrors ADD COLUMN hook_id bigint;
ALTER TABLE lager_github_mirrors ADD COLUMN hook_secret text;
ALTER TABLE lager_github_mirrors ADD COLUMN hook_checked_at timestamptz;
"#,
];

/// SSH keys a user can have at the same time.
const MAX_SSH_KEYS: i64 = 100;

const SELECT_SSH_KEYS: &str = "
SELECT id,
       name,
       fingerprint,
       public_key,
       (extract(epoch FROM created_at) * 1000)::bigint,
       (extract(epoch FROM last_used_at) * 1000)::bigint
FROM lager_ssh_keys
WHERE user_name = $1 AND ($2::bigint IS NULL OR id = $2)
ORDER BY id";

type SshKeyRow = (SshKeyId, String, String, String, i64, Option<i64>);

/// Key id, user, used more than a minute ago, repository, level.
type SshKeyAccessRow = (SshKeyId, String, bool, Option<String>, Option<String>);

/// Tokens a user can have at the same time.
const MAX_TOKENS: i64 = 100;

/// The user's and the token's access levels for a valid token of a user;
/// no rows for a wrong, expired, or deleted token. The last two columns tell
/// whether the token was used in the last minute and whether a level is the
/// token's.
const SELECT_TOKEN_ACCESS: &str = "
WITH t AS (
    SELECT id, user_name,
           last_used_at IS NULL OR last_used_at < now() - interval '1 minute' AS stale
    FROM lager_tokens
    WHERE token_sha256 = $1 AND user_name = $2
      AND (expires_at IS NULL OR expires_at > now())
)
SELECT t.id, t.stale, false, a.repository, a.level
FROM t LEFT JOIN lager_user_access a ON a.user_name = t.user_name
UNION ALL
SELECT t.id, t.stale, true, a.repository, a.level
FROM t JOIN lager_token_access a ON a.token_id = t.id";

/// Token id, used recently, a level of the token (not the user), repository,
/// level.
type TokenAccessRow = (TokenId, bool, bool, Option<String>, Option<String>);

/// A user's unexpired tokens with their levels; `$2` NULL for all tokens.
const SELECT_TOKENS: &str = "
SELECT t.id,
       t.name,
       (extract(epoch FROM t.created_at) * 1000)::bigint,
       (extract(epoch FROM t.expires_at) * 1000)::bigint,
       (extract(epoch FROM t.last_used_at) * 1000)::bigint,
       a.repository,
       a.level
FROM lager_tokens t
LEFT JOIN lager_token_access a ON a.token_id = t.id
WHERE t.user_name = $1
  AND ($2::bigint IS NULL OR t.id = $2)
  AND (t.expires_at IS NULL OR t.expires_at > now())
ORDER BY t.id";

type TokenRow = (
    TokenId,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
);

/// Serializes schema changes between servers that start together.
const MIGRATION_LOCK: i64 = 0x7275_7374_6769_7401;

/// Users with their access levels; `$1` NULL for all users.
const SELECT_USERS: &str = "
SELECT u.name,
       (extract(epoch FROM u.created_at) * 1000)::bigint,
       (extract(epoch FROM u.updated_at) * 1000)::bigint,
       u.updated_by,
       a.repository,
       a.level
FROM lager_users u
LEFT JOIN lager_user_access a ON a.user_name = u.name
WHERE $1::text IS NULL OR u.name = $1
ORDER BY u.name";

type UserRow = (String, i64, i64, String, Option<String>, Option<String>);

pub struct PgUsers {
    pub(super) pool: PgPool,
    verified: VerifiedHashes,
}

impl PgUsers {
    /// Connects and applies missing schema changes.
    pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<Self> {
        let options: PgConnectOptions = cfg
            .url
            .parse()
            .map_err(|e| anyhow::anyhow!("database.url: {e}"))?;
        let pool = PgPoolOptions::new()
            .max_connections(DATABASE_CONNECTIONS)
            .acquire_timeout(DATABASE_TIMEOUT)
            .connect_with(options)
            .await
            .map_err(|e| anyhow::anyhow!("database: {e}"))?;
        migrate(&pool)
            .await
            .map_err(|e| anyhow::anyhow!("database schema: {e}"))?;
        Ok(Self {
            pool,
            verified: VerifiedHashes::default(),
        })
    }

    /// The connection pool, shared with the pull requests
    /// ([`crate::pulls::Pulls`]).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn conn(&self) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, UserError> {
        self.pool.acquire().await.map_err(unavailable)
    }

    /// A personal access token in place of the password. The digest is
    /// looked up directly: the token is random, so it needs no slow hash.
    async fn authenticate_token(&self, user: &str, token: &str) -> Result<Identity, AuthError> {
        let digest = Sha256::digest(token.as_bytes()).to_vec();
        let rows: Vec<TokenAccessRow> = sqlx::query_as(SELECT_TOKEN_ACCESS)
            .bind(digest)
            .bind(user)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
        let Some(&(id, stale, ..)) = rows.first() else {
            return Err(AuthError::BadCredentials);
        };
        let (mut user_access, mut token_access) = (BTreeMap::new(), BTreeMap::new());
        for (_, _, of_token, repo, level) in rows {
            if let (Some(repo), Some(level)) = (repo, level) {
                let level = level
                    .parse::<Access>()
                    .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
                if of_token {
                    token_access.insert(repo, level);
                } else {
                    user_access.insert(repo, level);
                }
            }
        }
        if stale {
            // Off the request: a lost update only makes the time older.
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let r = sqlx::query("UPDATE lager_tokens SET last_used_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(&pool)
                    .await;
                if let Err(e) = r {
                    tracing::warn!(token = id, error = %e, "token use not recorded");
                }
            });
        }
        Ok(Identity::User {
            name: user.to_string(),
            access: user_access,
            token: (!token_access.is_empty()).then_some(token_access),
        })
    }
}

async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let mut tx = conn.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS lager_schema (
             version integer PRIMARY KEY,
             applied_at timestamptz NOT NULL DEFAULT now()
         )",
    )
    .execute(&mut *tx)
    .await?;
    let current: i32 = sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM lager_schema")
        .fetch_one(&mut *tx)
        .await?;
    let known = MIGRATIONS.len() as i32;
    anyhow::ensure!(
        current <= known,
        "the database has schema version {current}; this program knows up to {known}"
    );
    for (version, &sql) in (1..).zip(MIGRATIONS).skip(current as usize) {
        sqlx::raw_sql(sql).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO lager_schema (version) VALUES ($1)")
            .bind(version)
            .execute(&mut *tx)
            .await?;
        tracing::info!(version, "applied database schema change");
    }
    tx.commit().await?;
    Ok(())
}

impl PgUsers {
    /// Checks a user name and a password or token. `Ok(None)`: no such user.
    pub async fn authenticate(
        &self,
        user: &str,
        password: &str,
    ) -> Result<Option<Identity>, AuthError> {
        if password.starts_with(TOKEN_PREFIX) {
            return self.authenticate_token(user, password).await.map(Some);
        }
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT u.password_hash, a.repository, a.level
             FROM lager_users u
             LEFT JOIN lager_user_access a ON a.user_name = u.name
             WHERE u.name = $1",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
        let Some((hash, _, _)) = rows.first() else {
            return Ok(None);
        };
        if !self.verified.verify(user, password, hash).await {
            return Err(AuthError::BadCredentials);
        }
        let mut access = BTreeMap::new();
        for (_, repo, level) in rows {
            if let (Some(repo), Some(level)) = (repo, level) {
                let level = level
                    .parse::<Access>()
                    .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
                access.insert(repo, level);
            }
        }
        Ok(Some(Identity::User {
            name: user.to_string(),
            access,
            token: None,
        }))
    }

    /// The user with this name and its current access, for web sessions.
    pub async fn lookup(&self, name: &str) -> Result<Option<NamedUser>, AuthError> {
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT u.password_hash, a.repository, a.level
             FROM lager_users u
             LEFT JOIN lager_user_access a ON a.user_name = u.name
             WHERE u.name = $1",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
        let Some((hash, _, _)) = rows.first() else {
            return Ok(None);
        };
        let stamp = password_stamp(hash);
        let mut access = BTreeMap::new();
        for (_, repo, level) in rows {
            if let (Some(repo), Some(level)) = (repo, level) {
                let level = level
                    .parse::<Access>()
                    .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
                access.insert(repo, level);
            }
        }
        Ok(Some(NamedUser {
            identity: Identity::User {
                name: name.to_string(),
                access,
                token: None,
            },
            stamp,
        }))
    }

    /// All users, sorted by name.
    pub async fn list(&self) -> Result<Vec<User>, UserError> {
        users(&mut *self.conn().await?, None).await
    }

    pub async fn create(&self, new: NewUser, by: &str) -> Result<User, UserError> {
        let mut conn = self.conn().await?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        let inserted = sqlx::query(
            "INSERT INTO lager_users (name, password_hash, updated_by)
             VALUES ($1, $2, $3)
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(&new.name)
        .bind(&new.password_hash)
        .bind(by)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        if inserted.rows_affected() == 0 {
            return Err(UserError::Exists(new.name));
        }
        set_access(&mut tx, &new.name, &new.access).await?;
        let user = one(&mut tx, &new.name).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(user)
    }

    pub async fn update(
        &self,
        name: &str,
        change: UserChange,
        by: &str,
    ) -> Result<User, UserError> {
        let mut conn = self.conn().await?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        // Also locks the row until the end of the change.
        let updated = sqlx::query(
            "UPDATE lager_users
             SET password_hash = coalesce($2, password_hash),
                 updated_at = now(),
                 updated_by = $3
             WHERE name = $1",
        )
        .bind(name)
        .bind(&change.password_hash)
        .bind(by)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        if updated.rows_affected() == 0 {
            return Err(UserError::NotFound(name.into()));
        }
        if let Some(access) = &change.access {
            sqlx::query("DELETE FROM lager_user_access WHERE user_name = $1")
                .bind(name)
                .execute(&mut *tx)
                .await
                .map_err(unavailable)?;
            set_access(&mut tx, name, access).await?;
        }
        let user = one(&mut tx, name).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(user)
    }

    pub async fn delete(&self, name: &str, _by: &str) -> Result<(), UserError> {
        let deleted = sqlx::query("DELETE FROM lager_users WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        if deleted.rows_affected() == 0 {
            return Err(UserError::NotFound(name.into()));
        }
        Ok(())
    }

    /// Creates a personal access token for a user. Answers with the token,
    /// which is not kept.
    pub async fn create_token(
        &self,
        user: &str,
        new: NewToken,
    ) -> Result<(Token, String), UserError> {
        let secret = format!(
            "{TOKEN_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
        );
        let digest = Sha256::digest(secret.as_bytes()).to_vec();
        let mut conn = self.conn().await?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        sqlx::query("DELETE FROM lager_tokens WHERE user_name = $1 AND expires_at <= now()")
            .bind(user)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lager_tokens WHERE user_name = $1")
                .bind(user)
                .fetch_one(&mut *tx)
                .await
                .map_err(unavailable)?;
        if count >= MAX_TOKENS {
            return Err(UserError::Invalid(format!(
                "a user can have at most {MAX_TOKENS} tokens; delete one first"
            )));
        }
        let id: Option<TokenId> = sqlx::query_scalar(
            "INSERT INTO lager_tokens (user_name, name, token_sha256, expires_at)
             SELECT name, $2, $3, now() + make_interval(days => $4)
             FROM lager_users WHERE name = $1
             RETURNING id",
        )
        .bind(user)
        .bind(&new.name)
        .bind(digest)
        .bind(new.expires_days.map(|d| d as i32))
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        let Some(id) = id else {
            return Err(UserError::NotFound(user.into()));
        };
        if let Some(access) = &new.access {
            let (repos, levels) = columns(access);
            sqlx::query(
                "INSERT INTO lager_token_access (token_id, repository, level)
                 SELECT $1, r, l FROM unnest($2::text[], $3::text[]) AS t (r, l)",
            )
            .bind(id)
            .bind(repos)
            .bind(levels)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        }
        let token = tokens(&mut tx, user, Some(id))
            .await?
            .pop()
            .ok_or(UserError::TokenNotFound(id))?;
        tx.commit().await.map_err(unavailable)?;
        Ok((token, secret))
    }

    /// A user's tokens, oldest first, without expired ones.
    pub async fn tokens(&self, user: &str) -> Result<Vec<Token>, UserError> {
        tokens(&mut *self.conn().await?, user, None).await
    }

    pub async fn delete_token(&self, user: &str, id: TokenId) -> Result<(), UserError> {
        let deleted = sqlx::query("DELETE FROM lager_tokens WHERE id = $1 AND user_name = $2")
            .bind(id)
            .bind(user)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        if deleted.rows_affected() == 0 {
            return Err(UserError::TokenNotFound(id));
        }
        Ok(())
    }
}

impl PgUsers {
    /// The user of an SSH key, with the user's access. `Ok(None)`: no user
    /// has this key.
    pub async fn authenticate_ssh_key(
        &self,
        fingerprint: &str,
    ) -> Result<Option<Identity>, AuthError> {
        let rows: Vec<SshKeyAccessRow> = sqlx::query_as(
            "SELECT k.id, k.user_name,
                    k.last_used_at IS NULL OR k.last_used_at < now() - interval '1 minute',
                    a.repository, a.level
             FROM lager_ssh_keys k
             LEFT JOIN lager_user_access a ON a.user_name = k.user_name
             WHERE k.fingerprint = $1",
        )
        .bind(fingerprint)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
        let Some((id, name, stale, ..)) = rows.first().cloned() else {
            return Ok(None);
        };
        let mut access = BTreeMap::new();
        for (.., repo, level) in rows {
            if let (Some(repo), Some(level)) = (repo, level) {
                let level = level
                    .parse::<Access>()
                    .map_err(|e| AuthError::Unavailable(format!("database: {e}")))?;
                access.insert(repo, level);
            }
        }
        if stale {
            // Off the request: a lost update only makes the time older.
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let r = sqlx::query("UPDATE lager_ssh_keys SET last_used_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(&pool)
                    .await;
                if let Err(e) = r {
                    tracing::warn!(ssh_key = id, error = %e, "SSH key use not recorded");
                }
            });
        }
        Ok(Some(Identity::User {
            name,
            access,
            token: None,
        }))
    }

    /// Adds a public key to a user. A key belongs to at most one user.
    pub async fn add_ssh_key(
        &self,
        user: &str,
        name: &str,
        key: &PublicKeyLine,
    ) -> Result<SshKey, UserError> {
        let fingerprint = key.fingerprint();
        let mut conn = self.conn().await?;
        let mut tx = conn.begin().await.map_err(unavailable)?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lager_ssh_keys WHERE user_name = $1")
                .bind(user)
                .fetch_one(&mut *tx)
                .await
                .map_err(unavailable)?;
        if count >= MAX_SSH_KEYS {
            return Err(UserError::Invalid(format!(
                "a user can have at most {MAX_SSH_KEYS} SSH keys; delete one first"
            )));
        }
        let id: Option<SshKeyId> = sqlx::query_scalar(
            "INSERT INTO lager_ssh_keys (user_name, name, fingerprint, public_key)
             SELECT name, $2, $3, $4 FROM lager_users WHERE name = $1
             ON CONFLICT (fingerprint) DO NOTHING
             RETURNING id",
        )
        .bind(user)
        .bind(name)
        .bind(&fingerprint)
        .bind(key.openssh())
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        let Some(id) = id else {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lager_users WHERE name = $1)")
                    .bind(user)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(unavailable)?;
            return Err(if exists {
                UserError::Invalid(format!("the SSH key {fingerprint} is already added"))
            } else {
                UserError::NotFound(user.into())
            });
        };
        let key = ssh_keys(&mut tx, user, Some(id))
            .await?
            .pop()
            .ok_or(UserError::SshKeyNotFound(id))?;
        tx.commit().await.map_err(unavailable)?;
        Ok(key)
    }

    /// A user's SSH keys, oldest first.
    pub async fn ssh_keys(&self, user: &str) -> Result<Vec<SshKey>, UserError> {
        ssh_keys(&mut *self.conn().await?, user, None).await
    }

    pub async fn delete_ssh_key(&self, user: &str, id: SshKeyId) -> Result<(), UserError> {
        let deleted = sqlx::query("DELETE FROM lager_ssh_keys WHERE id = $1 AND user_name = $2")
            .bind(id)
            .bind(user)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
        if deleted.rows_affected() == 0 {
            return Err(UserError::SshKeyNotFound(id));
        }
        Ok(())
    }
}

async fn ssh_keys(
    conn: &mut PgConnection,
    user: &str,
    id: Option<SshKeyId>,
) -> Result<Vec<SshKey>, UserError> {
    let rows: Vec<SshKeyRow> = sqlx::query_as(SELECT_SSH_KEYS)
        .bind(user)
        .bind(id)
        .fetch_all(conn)
        .await
        .map_err(unavailable)?;
    Ok(rows
        .into_iter()
        .map(
            |(id, name, fingerprint, public_key, created_ms, last_used_ms)| SshKey {
                id,
                name,
                fingerprint,
                public_key,
                created_ms: created_ms as u64,
                last_used_ms: last_used_ms.map(|v| v as u64),
            },
        )
        .collect())
}

async fn tokens(
    conn: &mut PgConnection,
    user: &str,
    id: Option<TokenId>,
) -> Result<Vec<Token>, UserError> {
    let rows: Vec<TokenRow> = sqlx::query_as(SELECT_TOKENS)
        .bind(user)
        .bind(id)
        .fetch_all(conn)
        .await
        .map_err(unavailable)?;
    let mut out: Vec<Token> = Vec::new();
    for (id, name, created_ms, expires_ms, last_used_ms, repo, level) in rows {
        if out.last().is_none_or(|t| t.id != id) {
            out.push(Token {
                id,
                name,
                access: None,
                created_ms: created_ms as u64,
                expires_ms: expires_ms.map(|v| v as u64),
                last_used_ms: last_used_ms.map(|v| v as u64),
            });
        }
        if let (Some(repo), Some(level)) = (repo, level) {
            let level = level.parse::<Access>().map_err(UserError::Unavailable)?;
            let token = out.last_mut().expect("pushed");
            token.access.get_or_insert_default().insert(repo, level);
        }
    }
    Ok(out)
}

async fn set_access(
    conn: &mut PgConnection,
    name: &str,
    access: &BTreeMap<String, Access>,
) -> Result<(), UserError> {
    let (repos, levels) = columns(access);
    sqlx::query(
        "INSERT INTO lager_user_access (user_name, repository, level)
         SELECT $1, r, l FROM unnest($2::text[], $3::text[]) AS t (r, l)",
    )
    .bind(name)
    .bind(repos)
    .bind(levels)
    .execute(conn)
    .await
    .map_err(unavailable)?;
    Ok(())
}

async fn one(conn: &mut PgConnection, name: &str) -> Result<User, UserError> {
    users(conn, Some(name))
        .await?
        .pop()
        .ok_or_else(|| UserError::NotFound(name.into()))
}

async fn users(conn: &mut PgConnection, name: Option<&str>) -> Result<Vec<User>, UserError> {
    let rows: Vec<UserRow> = sqlx::query_as(SELECT_USERS)
        .bind(name)
        .fetch_all(conn)
        .await
        .map_err(unavailable)?;
    let mut out: Vec<User> = Vec::new();
    for (name, created_ms, updated_ms, updated_by, repo, level) in rows {
        if out.last().is_none_or(|u| u.name != name) {
            out.push(User {
                name,
                access: BTreeMap::new(),
                created_ms: Some(created_ms as u64),
                updated_ms: Some(updated_ms as u64),
                updated_by: Some(updated_by),
            });
        }
        if let (Some(repo), Some(level)) = (repo, level) {
            let level = level.parse::<Access>().map_err(UserError::Unavailable)?;
            out.last_mut().expect("pushed").access.insert(repo, level);
        }
    }
    Ok(out)
}

/// Repository names and levels as two arrays for `unnest`.
fn columns(access: &BTreeMap<String, Access>) -> (Vec<&str>, Vec<&str>) {
    access.iter().map(|(r, l)| (r.as_str(), l.as_str())).unzip()
}

fn unavailable(e: sqlx::Error) -> UserError {
    UserError::Unavailable(e.to_string())
}
