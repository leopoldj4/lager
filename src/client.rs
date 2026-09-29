//! The command-line client for the admin API: `rustgit login` keeps a
//! server URL and credentials, and repository commands then go through the
//! server instead of the storage service, so no storage keys are needed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// A server and the credentials for it, as `rustgit login` stores them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Login {
    /// Without credentials or a trailing `/`.
    pub url: String,
    pub user: String,
    pub password: String,
}

impl Login {
    /// `$XDG_CONFIG_HOME/rustgit/login.toml`, or `~/.config/rustgit/login.toml`.
    pub fn default_path() -> Option<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(base.join("rustgit").join("login.toml"))
    }

    pub fn load(path: &Path) -> anyhow::Result<Option<Login>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(toml::from_str(&text).map_err(|e| {
                anyhow::anyhow!("{}: {e}; run rustgit login again", path.display())
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::anyhow!("{}: {e}", path.display())),
        }
    }

    /// Writes the file readable only by its owner: it holds the password.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(toml::to_string(self)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Splits `https://user:password@host/` into the plain URL and the user
/// and password it names. Only `http` and `https` are accepted.
pub fn parse_server_url(url: &str) -> anyhow::Result<(String, Option<String>, Option<String>)> {
    let (scheme, rest) = url
        .split_once("://")
        .filter(|(s, _)| *s == "https" || *s == "http")
        .ok_or_else(|| anyhow::anyhow!("server URL must start with https:// or http://"))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (userinfo, host) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    anyhow::ensure!(!host.is_empty(), "server URL has no host");
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
            None => (Some(u.to_string()), None),
        },
        None => (None, None),
    };
    let path = path.trim_end_matches('/');
    let url = if path.is_empty() {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}/{path}")
    };
    Ok((url, user.filter(|u| !u.is_empty()), password))
}

#[derive(Clone, Debug, Deserialize)]
pub struct Me {
    pub name: String,
    pub admin: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Repo {
    pub name: String,
    pub id: String,
    pub default_branch: String,
    pub generation: Option<u64>,
}

/// An error answer of the server, or a failed request.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("wrong user name or password for {0}")]
    Unauthorized(String),
    #[error("{message} (HTTP {status})")]
    Server { status: u16, message: String },
    #[error("cannot reach {url}: {message}")]
    Unreachable { url: String, message: String },
}

pub struct Client {
    login: Login,
    http: reqwest::Client,
}

impl Client {
    pub fn new(login: Login) -> anyhow::Result<Client> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Client { login, http })
    }

    pub fn login(&self) -> &Login {
        &self.login
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<Option<T>, ClientError> {
        let url = format!("{}/_rustgit/api/{path}", self.login.url);
        let mut req = self
            .http
            .request(method, &url)
            .basic_auth(&self.login.user, Some(&self.login.password));
        if let Some(b) = body {
            req = req
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let unreachable = |e: reqwest::Error| {
            // The top-level message alone ("error sending request") names no cause.
            let e = e.without_url();
            let mut message = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                message = format!("{message}: {s}");
                source = s.source();
            }
            ClientError::Unreachable {
                url: self.login.url.clone(),
                message,
            }
        };
        let resp = req.send().await.map_err(unreachable)?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(unreachable)?;
        if status == 401 {
            return Err(ClientError::Unauthorized(self.login.url.clone()));
        }
        if !(200..300).contains(&status) {
            #[derive(Deserialize)]
            struct Answer {
                error: String,
            }
            let message = serde_json::from_slice::<Answer>(&bytes)
                .map(|a| a.error)
                .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).trim().to_string());
            return Err(ClientError::Server { status, message });
        }
        if bytes.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| ClientError::Server {
                status,
                message: format!("unexpected answer: {e}"),
            })
    }

    async fn call_some<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, ClientError> {
        self.call(method, path, body)
            .await?
            .ok_or_else(|| ClientError::Server {
                status: 204,
                message: "empty answer".into(),
            })
    }

    /// Checks the credentials.
    pub async fn me(&self) -> Result<Me, ClientError> {
        self.call_some(reqwest::Method::GET, "me", None).await
    }

    pub async fn repos(&self) -> Result<Vec<Repo>, ClientError> {
        self.call_some(reqwest::Method::GET, "repos", None).await
    }

    pub async fn create_repo(&self, name: &str, default_branch: &str) -> Result<Repo, ClientError> {
        let body = serde_json::json!({"name": name, "default_branch": default_branch});
        self.call_some(reqwest::Method::POST, "repos", Some(body))
            .await
    }

    pub async fn set_default_branch(&self, name: &str, branch: &str) -> Result<Repo, ClientError> {
        let body = serde_json::json!({"default_branch": branch});
        self.call_some(reqwest::Method::PATCH, &format!("repos/{name}"), Some(body))
            .await
    }

    pub async fn delete_repo(&self, name: &str) -> Result<(), ClientError> {
        self.call::<serde_json::Value>(reqwest::Method::DELETE, &format!("repos/{name}"), None)
            .await
            .map(|_| ())
    }
}

/// Reads a line from the terminal, or from standard input when it is not a
/// terminal. With `hidden`, the terminal does not show what is typed.
pub fn prompt(label: &str, hidden: bool) -> anyhow::Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    if terminal {
        eprint!("{label}");
        std::io::stderr().flush()?;
    }
    let restore = if terminal && hidden { echo_off() } else { None };
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if let Some(saved) = restore {
        // SAFETY: restores the attributes read from the same descriptor.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved) };
    }
    anyhow::ensure!(
        read? > 0,
        "no input for {}",
        label.trim_end_matches([':', ' '])
    );
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Turns off terminal echo; returns the previous attributes.
fn echo_off() -> Option<libc::termios> {
    // SAFETY: `t` is a valid out pointer for the attributes of stdin.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
            return None;
        }
        let saved = t;
        t.c_lflag &= !libc::ECHO;
        t.c_lflag |= libc::ECHONL;
        (libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) == 0).then_some(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls_are_split_into_url_user_and_password() {
        assert_eq!(
            parse_server_url("https://git.example.com/").unwrap(),
            ("https://git.example.com".into(), None, None)
        );
        assert_eq!(
            parse_server_url("https://leo@git.example.com").unwrap(),
            ("https://git.example.com".into(), Some("leo".into()), None)
        );
        assert_eq!(
            parse_server_url("http://a:b:c@127.0.0.1:8080/git/").unwrap(),
            (
                "http://127.0.0.1:8080/git".into(),
                Some("a".into()),
                Some("b:c".into())
            )
        );
        for bad in ["git.example.com", "ftp://x", "https://", "https://u@"] {
            assert!(parse_server_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_login_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustgit/login.toml");
        assert_eq!(Login::load(&path).unwrap(), None);
        let login = Login {
            url: "https://git.example.com".into(),
            user: "leo".into(),
            password: "pw".into(),
        };
        login.save(&path).unwrap();
        login.save(&path).unwrap();
        assert_eq!(Login::load(&path).unwrap(), Some(login));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
