//! Pull requests of GitHub mirrors (docs/decisions/0026-github-pull-requests.md).
//!
//! After the refs, a mirror sync copies GitHub's pull requests and their
//! comments through the REST API into the pull request tables, where the
//! pages and the live connection find them. Three passes share a budget of
//! pages per sync:
//!
//! 1. pull requests changed since the last sync, newest first, until one
//!    that was seen;
//! 2. while the first import runs, the next pages of all pull requests,
//!    newest first, so it continues where the last sync stopped;
//! 3. once that is done, comments changed since the last one seen, oldest
//!    first.
//!
//! Requests send the last answer's ETag; GitHub answers an unchanged list
//! with 304, which costs none of the token's rate limit.

use reqwest::StatusCode;
use reqwest::header::{ETAG, IF_NONE_MATCH};
use serde::Deserialize;

use super::{API, Github, GithubError};
use lager_core::github::{Due, PullCursors};
use lager_core::limits;
use lager_core::model::Oid;
use lager_core::pulls::{GithubComment, GithubPull, PullError, PullState};

const PER_PAGE: usize = 100;

#[derive(Deserialize)]
struct ApiUser {
    login: String,
}

#[derive(Deserialize)]
struct ApiRepo {
    full_name: String,
}

#[derive(Deserialize)]
struct ApiBranch {
    #[serde(rename = "ref")]
    name: String,
    /// `owner:branch`; `None` for a deleted fork.
    label: Option<String>,
    sha: String,
    /// `None` for a deleted fork.
    repo: Option<ApiRepo>,
}

#[derive(Deserialize)]
struct ApiPull {
    number: u64,
    html_url: String,
    title: String,
    body: Option<String>,
    /// `None` for a deleted account.
    user: Option<ApiUser>,
    state: String,
    head: ApiBranch,
    base: ApiBranch,
    merged_at: Option<String>,
    merge_commit_sha: Option<String>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
}

#[derive(Deserialize)]
struct ApiPullDetail {
    merged_by: Option<ApiUser>,
}

#[derive(Deserialize)]
struct ApiComment {
    id: i64,
    /// `…/pull/<n>#issuecomment-…` on pull requests, `…/issues/<n>#…` on
    /// issues.
    html_url: String,
    user: Option<ApiUser>,
    body: Option<String>,
    created_at: String,
    updated_at: String,
}

/// GitHub's name for a deleted account.
const GHOST: &str = "ghost";

fn login(u: Option<ApiUser>) -> String {
    u.map_or_else(|| GHOST.to_string(), |u| u.login)
}

enum Page<T> {
    /// The ETag matched.
    Unchanged,
    Fresh {
        items: Vec<T>,
        etag: Option<String>,
    },
}

impl Github {
    /// Copies the mirror's pull requests and comments from GitHub. The
    /// cursors are kept after an error, so the next sync goes on from
    /// there.
    pub(super) async fn sync_pulls(&self, due: &Due, token: &str) -> Result<(), String> {
        let Some(mut cursors) = self.db.cursors(due.repo).await.map_err(|e| e.to_string())? else {
            return Ok(());
        };
        let before = cursors.clone();
        let result = self.sync_pulls_with(due, token, &mut cursors).await;
        if cursors != before
            && let Err(e) = self.db.set_cursors(due.repo, &cursors).await
        {
            tracing::warn!(repo = %due.name, error = %e, "pull request import not recorded");
        }
        result
    }

    async fn sync_pulls_with(
        &self,
        due: &Due,
        token: &str,
        c: &mut PullCursors,
    ) -> Result<(), String> {
        let gh = &due.github;
        let mut pages = limits::GITHUB_API_PAGES_PER_SYNC;

        // 1. Changed since the last sync.
        let mut seen = Vec::new();
        let mut caught_up = false;
        let mut etag = None;
        for page in 1.. {
            if pages == 0 {
                break;
            }
            pages -= 1;
            let path = format!(
                "/repos/{gh}/pulls?state=all&sort=updated&direction=desc&per_page={PER_PAGE}&page={page}"
            );
            let known = if page == 1 {
                c.pulls_etag.as_deref()
            } else {
                None
            };
            let (items, tag) = match self.api_page::<ApiPull>(token, &path, known).await? {
                // Every change moves a pull request to the first page.
                Page::Unchanged => {
                    caught_up = true;
                    etag = c.pulls_etag.clone();
                    break;
                }
                Page::Fresh { items, etag } => (items, etag),
            };
            if page == 1 {
                etag = tag;
            }
            let full = items.len() == PER_PAGE;
            for p in items {
                if c.pulls_since.as_ref().is_some_and(|s| p.updated_at <= *s) {
                    caught_up = true;
                    break;
                }
                seen.push(p);
            }
            // Without a cursor, the first page sets it; the import reads
            // the rest.
            if caught_up || !full || c.pulls_since.is_none() {
                caught_up = true;
                break;
            }
        }
        // Oldest first, so a pull request closed for a newer one with the
        // same branches is closed first.
        seen.reverse();
        let newest = seen.last().map(|p| p.updated_at.clone());
        for p in seen {
            self.import_pull(due, token, p, true).await?;
        }
        if caught_up {
            if let Some(n) = newest
                && c.pulls_since.as_ref().is_none_or(|s| n > *s)
            {
                c.pulls_since = Some(n);
            }
            c.pulls_etag = etag;
        } else {
            // The rest of the changes comes in the next sync, from the
            // same cursor.
            c.pulls_etag = None;
        }

        // 2. The first import, newest first. New pull requests move the
        // pages along, which only repeats some.
        while let Some(page) = c.import_page {
            if pages == 0 {
                break;
            }
            pages -= 1;
            let path = format!(
                "/repos/{gh}/pulls?state=all&sort=created&direction=desc&per_page={PER_PAGE}&page={page}"
            );
            let Page::Fresh { items, .. } = self.api_page::<ApiPull>(token, &path, None).await?
            else {
                break;
            };
            let full = items.len() == PER_PAGE;
            for p in items {
                self.import_pull(due, token, p, false).await?;
            }
            c.import_page = full.then_some(page + 1);
            if !full {
                tracing::info!(repo = %due.name, github = %gh, "pull requests imported");
            }
        }
        if c.import_page.is_some() {
            // Comments wait for their pull requests.
            return Ok(());
        }

        // 3. Comments changed since the last one seen. `since` includes
        // that time, so the last ones come again and an unchanged answer
        // matches the ETag.
        while pages > 0 {
            pages -= 1;
            let mut path = format!(
                "/repos/{gh}/issues/comments?sort=updated&direction=asc&per_page={PER_PAGE}&page={}",
                c.comments_page
            );
            if let Some(s) = &c.comments_since {
                path.push_str("&since=");
                path.push_str(s);
            }
            let (items, tag) = match self
                .api_page::<ApiComment>(token, &path, c.comments_etag.as_deref())
                .await?
            {
                Page::Unchanged => break,
                Page::Fresh { items, etag } => (items, etag),
            };
            let full = items.len() == PER_PAGE;
            let mut last = None;
            let mut waiting = false;
            for comment in items {
                if let Some(number) = pull_of_comment(&comment.html_url) {
                    let imported = GithubComment {
                        number,
                        id: comment.id,
                        author: login(comment.user),
                        body: comment.body.unwrap_or_default(),
                        created_at: comment.created_at,
                        updated_at: comment.updated_at.clone(),
                    };
                    match self
                        .pulls
                        .db
                        .import_github_comment(due.repo, &imported)
                        .await
                    {
                        Ok(Some(p)) => self.pulls.publish(due.repo, &p),
                        Ok(None) => {}
                        // A pull request newer than this sync's list: the
                        // next sync reads it first.
                        Err(PullError::NotFound) => {
                            waiting = true;
                            break;
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
                // Comments on issues are skipped.
                last = Some(comment.updated_at);
            }
            let moved = last
                .as_ref()
                .is_some_and(|l| c.comments_since.as_ref().is_none_or(|s| l > s));
            if moved {
                c.comments_since = last;
                c.comments_page = 1;
                c.comments_etag = None;
            } else if full && !waiting {
                // A full page of comments with the same time: the next
                // page.
                c.comments_page += 1;
                c.comments_etag = None;
            } else if !waiting {
                c.comments_etag = tag;
            }
            if waiting || !full {
                break;
            }
        }
        Ok(())
    }

    /// Writes one pull request. `live`: it changed since the last sync, so
    /// a new merge is worth one request for who merged it.
    async fn import_pull(
        &self,
        due: &Due,
        token: &str,
        p: ApiPull,
        live: bool,
    ) -> Result<(), String> {
        let merged = p.merged_at.is_some();
        let state = match (p.state.as_str(), merged) {
            (_, true) => PullState::Merged,
            ("open", _) => PullState::Open,
            _ => PullState::Closed,
        };
        let mut merged_by = None;
        if merged && live {
            let old = self
                .pulls
                .db
                .get(due.repo, p.number)
                .await
                .map_err(|e| e.to_string())?;
            if old.is_some_and(|o| o.state != PullState::Merged) {
                let path = format!("/repos/{}/pulls/{}", due.github, p.number);
                merged_by = match self.api::<ApiPullDetail>(token, &path).await {
                    Ok(d) => d.merged_by.map(|u| u.login),
                    Err(e) => return Err(api_message(e)),
                };
            }
        }
        // Branches of forks are named after their owner.
        let same_repo = p
            .head
            .repo
            .as_ref()
            .is_some_and(|r| r.full_name.eq_ignore_ascii_case(&due.github));
        let source = match p.head.label {
            Some(label) if !same_repo => label,
            // GitHub shows "unknown repository" for a deleted fork.
            None if !same_repo => format!("unknown:{}", p.head.name),
            _ => p.head.name,
        };
        let merge_oid = p.merge_commit_sha.filter(|_| merged);
        let merge_base = self
            .merge_base(due, &p.base.sha, &p.head.sha)
            .await
            .unwrap_or_else(|| p.base.sha.clone());
        let pull = GithubPull {
            number: p.number,
            url: p.html_url,
            title: p.title,
            body: p.body.unwrap_or_default(),
            author: login(p.user),
            source,
            target: p.base.name,
            state,
            head_oid: p.head.sha,
            target_oid: merge_oid.clone().unwrap_or(p.base.sha),
            merge_base,
            merge_oid,
            merged_by,
            created_at: p.created_at,
            updated_at: p.updated_at,
            closed_at: p.closed_at,
        };
        match self.pulls.db.import_github(due.repo, &pull).await {
            Ok(Some(changed)) => self.pulls.publish(due.repo, &changed),
            Ok(None) => {}
            Err(e @ PullError::State(_)) => {
                tracing::warn!(repo = %due.name, pull = pull.number, error = %e, "pull request not imported");
            }
            Err(e) => return Err(e.to_string()),
        }
        Ok(())
    }

    /// The merge base of GitHub's base and head commits, if the cache has
    /// both. Without it, the comparison starts at the base commit.
    async fn merge_base(&self, due: &Due, base: &str, head: &str) -> Option<String> {
        let view = self
            .registry
            .controller(due.repo)
            .prepare_read()
            .await
            .ok()?;
        let format = view.format();
        let (base, head) = (
            Oid::parse(format, base).ok()?,
            Oid::parse(format, head).ok()?,
        );
        let git = &self.registry.svc.git;
        git.merge_base(&view.dir, format, &base, &head)
            .await
            .ok()
            .flatten()
            .map(|o| o.to_string())
    }

    /// One page of a GitHub list. `etag`: the last answer's, for a 304.
    async fn api_page<T: serde::de::DeserializeOwned>(
        &self,
        token: &str,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Page<T>, String> {
        let mut req = self
            .http
            .get(format!("{API}{path}"))
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28");
        if let Some(e) = etag {
            req = req.header(IF_NONE_MATCH, e);
        }
        let res = req.send().await.map_err(|e| api_message(e.into()))?;
        if res.status() == StatusCode::NOT_MODIFIED {
            return Ok(Page::Unchanged);
        }
        if let Some(m) = rate_limited(&res) {
            return Err(m);
        }
        let res = res.error_for_status().map_err(|e| api_message(e.into()))?;
        let etag = res
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let items = res
            .json()
            .await
            .map_err(|e| format!("GitHub answered unexpectedly: {e}"))?;
        Ok(Page::Fresh { items, etag })
    }
}

/// The pull request of a comment's page, `…/pull/<n>#issuecomment-…`.
fn pull_of_comment(html_url: &str) -> Option<u64> {
    let (_, rest) = html_url.rsplit_once("/pull/")?;
    rest.split(['#', '/', '?']).next()?.parse().ok()
}

/// A message for a used-up rate limit, with the minutes until it resets.
fn rate_limited(res: &reqwest::Response) -> Option<String> {
    let s = res.status();
    if s != StatusCode::FORBIDDEN && s != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let header = |n: &str| {
        res.headers()
            .get(n)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    };
    if header("x-ratelimit-remaining") != Some(0) && s != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let minutes = header("x-ratelimit-reset").map_or(1, |r| r.saturating_sub(now).div_ceil(60));
    Some(format!(
        "GitHub's API limit for your account is used up; it resets in {} minute{}",
        minutes.max(1),
        if minutes > 1 { "s" } else { "" }
    ))
}

fn api_message(e: GithubError) -> String {
    match e {
        GithubError::Revoked => {
            "GitHub refused access; connect your GitHub account again in Settings".into()
        }
        e => e.to_string(),
    }
}
