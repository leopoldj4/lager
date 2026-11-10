//! Changes to pull requests of GitHub mirrors
//! (docs/decisions/0028-two-way-github-mirrors.md).
//!
//! The pull requests of a mirror are GitHub's. Opening one, a comment,
//! closing, reopening, and merging here are requests to GitHub's API as the
//! user's own GitHub account, so GitHub checks its permissions and rules.
//! GitHub's answer is written like a sync writes it, which the next sync
//! then finds unchanged.

use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Github;
use super::pulls::{ApiComment, ApiPull, rate_limited};
use crate::pulls::PullOpError;
use lager_core::identity::Identity;
use lager_core::pulls::{Pull, validate_text};
use lager_core::repo::{Access, RepoSettings};

/// GitHub's answer to a refused request.
#[derive(Deserialize)]
struct ApiError {
    message: Option<String>,
    /// Texts, or objects with a `message` or with a `field` and a `code`.
    #[serde(default)]
    errors: Vec<Value>,
}

impl ApiError {
    /// The specific reasons, else the general one.
    fn text(self) -> String {
        let field = |e: &Value| {
            let name = match e.get("field")?.as_str()? {
                "head" => "the source branch",
                "base" => "the target branch",
                f => f,
            };
            Some(format!("{name} is {}", e.get("code")?.as_str()?))
        };
        let specific: Vec<String> = self
            .errors
            .iter()
            .filter_map(|e| {
                e.as_str()
                    .or_else(|| e.get("message")?.as_str())
                    .map(str::to_string)
                    .or_else(|| field(e))
            })
            .collect();
        if specific.is_empty() {
            self.message.unwrap_or_default()
        } else {
            specific.join("; ")
        }
    }
}

/// The GitHub side of a change by a user.
struct Acting {
    github: String,
    token: String,
    level: Access,
}

impl Github {
    /// The mirror's GitHub repository and the user's token for it.
    async fn acting(&self, who: &Identity, repo: &RepoSettings) -> Result<Acting, PullOpError> {
        let Identity::User { name: user, .. } = who else {
            return Err(PullOpError::SignedOut);
        };
        let level = self.directory.access(who, repo);
        if level < Access::Read {
            return Err(PullOpError::NotFound);
        }
        let Some(github) = repo.mirror.clone() else {
            return Err(PullOpError::Forbidden(
                "this repository no longer follows GitHub".into(),
            ));
        };
        let account = self
            .db
            .account(user)
            .await
            .map_err(|e| PullOpError::Unavailable(e.to_string()))?;
        let Some(account) = account else {
            return Err(PullOpError::Forbidden(
                "this pull request is on GitHub; connect your GitHub account in Settings to change it"
                    .into(),
            ));
        };
        Ok(Acting {
            github,
            token: account.token,
            level,
        })
    }

    /// The copy of GitHub's pull request `number`.
    async fn copied(&self, repo: &RepoSettings, number: u64) -> Result<Pull, PullOpError> {
        self.pulls
            .db
            .get(repo.id, number)
            .await?
            .filter(|p| p.github_url.is_some())
            .ok_or(PullOpError::NotFound)
    }

    /// Writes GitHub's answer and answers the copy.
    async fn written(
        &self,
        repo: &RepoSettings,
        a: &Acting,
        p: ApiPull,
    ) -> Result<Pull, PullOpError> {
        let number = p.number;
        self.import_pull(repo.id, &a.github, &a.token, p, true)
            .await
            .map_err(PullOpError::Unavailable)?;
        self.pulls
            .db
            .get(repo.id, number)
            .await?
            .ok_or(PullOpError::NotFound)
    }

    /// Opens a pull request on GitHub to merge `source` into `target`.
    pub async fn pull_create(
        &self,
        repo: &RepoSettings,
        who: &Identity,
        source: &str,
        target: &str,
        title: &str,
        body: &str,
    ) -> Result<Pull, PullOpError> {
        let a = self.acting(who, repo).await?;
        validate_text(Some(title), body)?;
        let p: ApiPull = self
            .change(
                Method::POST,
                &a.token,
                &format!("/repos/{}/pulls", a.github),
                json!({ "title": title.trim(), "body": body, "head": source, "base": target }),
            )
            .await?;
        tracing::info!(repo = %repo.name, pull = p.number, user = who.name(), "pull request opened on GitHub");
        let p = self.written(repo, &a, p).await?;
        // Its ref comes with the sync.
        self.poke(repo.id).await;
        Ok(p)
    }

    pub async fn pull_comment(
        &self,
        repo: &RepoSettings,
        who: &Identity,
        number: u64,
        body: &str,
    ) -> Result<Pull, PullOpError> {
        let a = self.acting(who, repo).await?;
        validate_text(None, body)?;
        if body.trim().is_empty() {
            return Err(PullOpError::Invalid("write a comment first".into()));
        }
        // GitHub numbers issues with the pull requests, and takes comments
        // on both.
        let p = self.copied(repo, number).await?;
        let comment: ApiComment = self
            .change(
                Method::POST,
                &a.token,
                &format!("/repos/{}/issues/{number}/comments", a.github),
                json!({ "body": body }),
            )
            .await?;
        Ok(self
            .import_comment(repo.id, number, comment)
            .await?
            .unwrap_or(p))
    }

    /// Closes a pull request on GitHub, or opens it again.
    pub async fn pull_set_open(
        &self,
        repo: &RepoSettings,
        who: &Identity,
        number: u64,
        open: bool,
    ) -> Result<Pull, PullOpError> {
        let a = self.acting(who, repo).await?;
        self.copied(repo, number).await?;
        let p: ApiPull = self
            .change(
                Method::PATCH,
                &a.token,
                &format!("/repos/{}/pulls/{number}", a.github),
                json!({ "state": if open { "open" } else { "closed" } }),
            )
            .await?;
        self.written(repo, &a, p).await
    }

    /// Merges an open pull request on GitHub whose head is still at
    /// `expected_head`. GitHub writes the commit; the sync brings it.
    pub async fn pull_merge(
        &self,
        repo: &RepoSettings,
        who: &Identity,
        number: u64,
        expected_head: &str,
        squash: bool,
    ) -> Result<Pull, PullOpError> {
        let a = self.acting(who, repo).await?;
        if a.level < Access::Write {
            return Err(PullOpError::Forbidden("merging needs write access".into()));
        }
        self.copied(repo, number).await?;
        let path = format!("/repos/{}/pulls/{number}", a.github);
        let _: Value = self
            .change(
                Method::PUT,
                &a.token,
                &format!("{path}/merge"),
                json!({
                    "sha": expected_head,
                    "merge_method": if squash { "squash" } else { "merge" },
                }),
            )
            .await?;
        tracing::info!(repo = %repo.name, pull = number, user = who.name(), squash, "pull request merged on GitHub");
        self.poke(repo.id).await;
        let merged = match self
            .change::<ApiPull>(Method::GET, &a.token, &path, Value::Null)
            .await
        {
            Ok(p) => self.written(repo, &a, p).await,
            Err(e) => Err(e),
        };
        // Merged on GitHub whatever the copy says; the sync writes it.
        merged.map_err(|e| {
            tracing::warn!(repo = %repo.name, pull = number, error = %e, "merge not recorded");
            PullOpError::Busy("merged on GitHub; the pull request shows it shortly".into())
        })
    }

    /// A request to GitHub's API that changes something. `body`: JSON, or
    /// null for none.
    async fn change<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        token: &str,
        path: &str,
        body: Value,
    ) -> Result<T, PullOpError> {
        let mut req = self.request(method, token, path);
        if !body.is_null() {
            req = req.json(&body);
        }
        let res = req
            .send()
            .await
            .map_err(|e| PullOpError::Busy(format!("GitHub did not answer: {e}")))?;
        let status = res.status();
        if status.is_success() {
            return res
                .json()
                .await
                .map_err(|e| PullOpError::Busy(format!("GitHub answered unexpectedly: {e}")));
        }
        if let Some(m) = rate_limited(&res) {
            return Err(PullOpError::Busy(m));
        }
        let why = res
            .json::<ApiError>()
            .await
            .map(ApiError::text)
            .unwrap_or_default();
        let said = |prefix: &str| {
            if why.is_empty() {
                prefix.to_string()
            } else {
                format!("{prefix}: {why}")
            }
        };
        Err(match status {
            StatusCode::UNAUTHORIZED => PullOpError::Forbidden(
                "GitHub refused access; connect your GitHub account again in Settings".into(),
            ),
            // The head moved since the page showed it.
            StatusCode::CONFLICT => PullOpError::Stale,
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::UNPROCESSABLE_ENTITY => {
                PullOpError::State(said("GitHub refused this"))
            }
            // GitHub hides repositories that an account cannot read.
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => {
                PullOpError::Forbidden(said("your GitHub account may not do this"))
            }
            s if s.is_server_error() => {
                PullOpError::Busy("GitHub is unavailable; retry later".into())
            }
            s => PullOpError::Rejected(said(&format!("GitHub answered {s}"))),
        })
    }
}
