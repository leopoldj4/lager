//! Browser sessions in signed cookies (docs/decisions/0014-web-interface.md).
//!
//! The cookie holds the user name, an expiry time, and an HMAC-SHA256 over
//! both and the user's password stamp. Nothing is stored on the server:
//! every server with the same secret accepts the cookie, and each use looks
//! the user up again, so changed access applies at once and a changed
//! password ends the session.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;

use crate::http::access::Directory;
use worktree_core::identity::{AuthError, Identity};
use worktree_core::password::{constant_time_eq, hmac_sha256};

pub const COOKIE: &str = "worktree_session";
/// The whole site: the API and the raw files under each repository. Git
/// requests carry the cookie too, but Git authentication ignores it.
pub const COOKIE_PATH: &str = "/";
/// Earlier cookies covered only the API; signing in and out removes them.
const OLD_COOKIE_PATH: &str = "/-/api";

pub struct Sessions {
    secret: Vec<u8>,
    lifetime_secs: u64,
    secure: bool,
}

impl Sessions {
    /// `secure`: mark cookies `Secure` (all but plain HTTP on loopback).
    pub fn new(secret: &str, days: u64, secure: bool) -> Self {
        Self {
            secret: secret.as_bytes().to_vec(),
            lifetime_secs: days * 24 * 3600,
            secure,
        }
    }

    fn mac(&self, user: &str, expires: u64, stamp: &[u8; 32]) -> [u8; 32] {
        hmac_sha256(
            &self.secret,
            &[
                b"worktree-session-v1\0",
                user.as_bytes(),
                b"\0",
                expires.to_string().as_bytes(),
                b"\0",
                stamp,
            ],
        )
    }

    /// The `Set-Cookie` value for a new session.
    pub fn issue(&self, user: &str, stamp: &[u8; 32]) -> String {
        let expires = now_secs() + self.lifetime_secs;
        let value = format!(
            "{}.{expires}.{}",
            B64.encode(user),
            B64.encode(self.mac(user, expires, stamp))
        );
        self.cookie(&value, self.lifetime_secs)
    }

    /// The `Set-Cookie` value that removes the session.
    pub fn clear(&self) -> String {
        self.cookie("", 0)
    }

    /// The `Set-Cookie` value that removes a session of an earlier path.
    pub fn clear_old(&self) -> String {
        self.cookie_at(OLD_COOKIE_PATH, "", 0)
    }

    fn cookie(&self, value: &str, max_age: u64) -> String {
        self.cookie_at(COOKIE_PATH, value, max_age)
    }

    fn cookie_at(&self, path: &str, value: &str, max_age: u64) -> String {
        format!(
            "{COOKIE}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Lax{}",
            if self.secure { "; Secure" } else { "" }
        )
    }

    /// The signed-in user for a cookie value. `Ok(None)`: no valid session.
    pub async fn resolve(
        &self,
        directory: &Directory,
        value: &str,
    ) -> Result<Option<Identity>, AuthError> {
        let mut parts = value.split('.');
        let (Some(user), Some(expires), Some(mac), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Ok(None);
        };
        let (Ok(user), Ok(expires), Ok(mac)) = (
            B64.decode(user).map(String::from_utf8),
            expires.parse::<u64>(),
            B64.decode(mac),
        ) else {
            return Ok(None);
        };
        let Ok(user) = user else { return Ok(None) };
        if expires <= now_secs() {
            return Ok(None);
        }
        let Some(found) = directory.lookup(&user).await? else {
            return Ok(None);
        };
        if !constant_time_eq(&self.mac(&user, expires, &found.stamp), &mac) {
            return Ok(None);
        }
        Ok(Some(found.identity))
    }
}

/// The session cookie of a request.
pub fn from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(COOKIE)?.strip_prefix('='))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn now_secs() -> u64 {
    worktree_core::model::now_ms() / 1000
}
