//! Salted password hashes for production credentials: PBKDF2-HMAC-SHA256.
//!
//! Format: `pbkdf2-sha256$<iterations>$<salt, base64>$<hash, base64>`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD as B64;
use sha2::{Digest, Sha256};

pub const DEFAULT_ITERATIONS: u32 = 600_000;
const PREFIX: &str = "pbkdf2-sha256";
const MIN_ITERATIONS: u32 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordHash {
    iterations: u32,
    salt: Vec<u8>,
    hash: Vec<u8>,
}

impl PasswordHash {
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split('$').collect();
        let [PREFIX, iterations, salt, hash] = parts[..] else {
            return Err(format!(
                "password hash must look like {PREFIX}$<iterations>$<salt>$<hash>"
            ));
        };
        let iterations: u32 = iterations
            .parse()
            .map_err(|_| "invalid iteration count".to_string())?;
        if iterations < MIN_ITERATIONS {
            return Err(format!("at least {MIN_ITERATIONS} iterations are required"));
        }
        let salt = B64.decode(salt).map_err(|_| "invalid salt".to_string())?;
        let hash = B64.decode(hash).map_err(|_| "invalid hash".to_string())?;
        if salt.len() < 16 || hash.len() != 32 {
            return Err("salt must have at least 16 bytes and hash 32 bytes".into());
        }
        Ok(Self {
            iterations,
            salt,
            hash,
        })
    }

    pub fn new(password: &str, iterations: u32) -> Self {
        let salt: [u8; 16] = rand::random();
        let hash = pbkdf2_sha256(password.as_bytes(), &salt, iterations).to_vec();
        Self {
            iterations,
            salt: salt.to_vec(),
            hash,
        }
    }

    pub fn verify(&self, password: &str) -> bool {
        let got = pbkdf2_sha256(password.as_bytes(), &self.salt, self.iterations);
        constant_time_eq(&got, &self.hash)
    }
}

impl std::fmt::Display for PasswordHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{PREFIX}${}${}${}",
            self.iterations,
            B64.encode(&self.salt),
            B64.encode(&self.hash)
        )
    }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// HMAC-SHA256 with the key already reduced to one block (RFC 2104).
struct Hmac {
    inner: Sha256,
    outer: Sha256,
}

impl Hmac {
    fn new(key: &[u8]) -> Self {
        let mut block = [0u8; 64];
        if key.len() > 64 {
            block[..32].copy_from_slice(&Sha256::digest(key));
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let mut inner = Sha256::new();
        let mut outer = Sha256::new();
        inner.update(block.map(|b| b ^ 0x36));
        outer.update(block.map(|b| b ^ 0x5c));
        Self { inner, outer }
    }

    fn mac(&self, data: &[&[u8]]) -> [u8; 32] {
        let mut inner = self.inner.clone();
        for d in data {
            inner.update(d);
        }
        let mut outer = self.outer.clone();
        outer.update(inner.finalize());
        outer.finalize().into()
    }
}

/// HMAC-SHA256 of the concatenated `data` (RFC 2104).
pub fn hmac_sha256(key: &[u8], data: &[&[u8]]) -> [u8; 32] {
    Hmac::new(key).mac(data)
}

/// PBKDF2-HMAC-SHA256 with a 32-byte result (RFC 8018).
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let hmac = Hmac::new(password);
    let mut u = hmac.mac(&[salt, &1u32.to_be_bytes()]);
    let mut out = u;
    for _ in 1..iterations {
        u = hmac.mac(&[&u]);
        for (o, x) in out.iter_mut().zip(u) {
            *o ^= x;
        }
    }
    out
}
