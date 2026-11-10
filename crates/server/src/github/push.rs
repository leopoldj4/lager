//! Pushes to GitHub mirrors
//! (docs/decisions/0028-two-way-github-mirrors.md).
//!
//! GitHub stays the source of a mirror. A push here goes to GitHub first,
//! as the pushing user's own GitHub account, and changes the refs here only
//! once GitHub accepted it ([`Forward`]). Every ref carries a lease on the
//! value it has here, so a push never replaces commits on GitHub that the
//! mirror has not seen. Whatever fails after GitHub accepted, the sync
//! brings the change like any other on GitHub.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Github, remote_message, url};
use crate::repository::{CacheInstance, Forward};
use lager_core::limits;
use lager_core::model::{Oid, RefUpdate, RepoId};

/// GitHub has a value that the mirror has not seen.
const STALE: &str = "GitHub has changes that are not here yet; fetch and push again";
const WORKFLOW_SCOPE: &str =
    "GitHub needs more access for workflow files; connect your GitHub account again in Settings";

impl Github {
    /// Where a push to the mirror `repo` of `github` goes first, with the
    /// pushing user's token.
    pub fn forward(self: &Arc<Self>, repo: RepoId, github: &str, token: &str) -> Forward {
        let (me, source, token) = (self.clone(), github.to_string(), token.to_string());
        let poked = self.clone();
        Forward {
            target: format!("github.com/{github}"),
            send: Arc::new(move |cache, updates, deadline| {
                let (me, source, token) = (me.clone(), source.clone(), token.clone());
                Box::pin(async move {
                    me.send(repo, &source, &token, &cache, &updates, deadline)
                        .await
                })
            }),
            resync: Arc::new(move || {
                let me = poked.clone();
                Box::pin(async move {
                    me.poke(repo).await;
                    // GitHub moves the head of a pull request a moment
                    // after its branch.
                    tokio::spawn(async move {
                        tokio::time::sleep(limits::GITHUB_FOLLOW_UP_SYNC).await;
                        me.poke(repo).await;
                    });
                })
            }),
        }
    }

    /// Asks for a sync of the mirror now.
    pub(super) async fn poke(&self, repo: RepoId) {
        match self.db.poke(repo, limits::GITHUB_SYNC_INTERVAL).await {
            Ok(true) => self.wake.notify_one(),
            Ok(false) => {}
            Err(e) => tracing::debug!(%repo, error = %e, "mirror not poked"),
        }
    }

    /// Pushes the updates to GitHub, all or none. `Err`: the refused refs
    /// with their reasons.
    async fn send(
        &self,
        repo: RepoId,
        github: &str,
        token: &str,
        cache: &CacheInstance,
        updates: &[RefUpdate],
        deadline: Instant,
    ) -> Result<(), Vec<(String, String)>> {
        let all = |why: String| -> Vec<(String, String)> {
            updates
                .iter()
                .map(|u| (u.name.to_string(), why.clone()))
                .collect()
        };
        let mut c = self.remote_command(Some(&cache.dir), token);
        c.args(["push", "--atomic", "--porcelain", "--no-verify"]);
        // The value here is the one the client's change was checked
        // against: GitHub takes the change only from that value, also a
        // force push or a deletion.
        for u in updates {
            c.arg(format!("--force-with-lease={}:{}", u.name, oid(&u.old)));
        }
        c.arg(url(github));
        for u in updates {
            c.arg(format!("{}:{}", oid(&u.new), u.name));
        }
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let refused = match tokio::time::timeout_at(deadline.into(), c.output()).await {
            Ok(Ok(out)) if out.status.success() => {
                tracing::info!(%repo, github, refs = updates.len(), "push forwarded to GitHub");
                return Ok(());
            }
            Ok(Ok(out)) => refusals(
                updates,
                &String::from_utf8_lossy(&out.stdout),
                &String::from_utf8_lossy(&out.stderr),
            ),
            Ok(Err(e)) => all(format!("git push to GitHub did not start: {e}")),
            Err(_) => all("GitHub did not answer in time".into()),
        };
        // A lost answer, or the repeat of a push that GitHub has: the
        // refs there say what happened.
        if let Ok(remote) = self.ls_refs(token, github, updates).await {
            let at = |u: &RefUpdate| remote.get(u.name.as_str()).map(String::as_str);
            if updates
                .iter()
                .all(|u| at(u) == u.new.as_ref().map(|o| o.as_str()))
            {
                tracing::info!(%repo, github, refs = updates.len(), "GitHub has the push already");
                return Ok(());
            }
            if updates
                .iter()
                .any(|u| at(u) != u.old.as_ref().map(|o| o.as_str()))
            {
                // The fetch after this refusal should find GitHub's commits.
                self.poke(repo).await;
            }
        }
        tracing::info!(%repo, github, reason = %refused[0].1, "GitHub refused a push");
        Err(refused)
    }

    /// The values on GitHub of the updates' refs; absent ones are left out.
    async fn ls_refs(
        &self,
        token: &str,
        github: &str,
        updates: &[RefUpdate],
    ) -> Result<HashMap<String, String>, String> {
        let mut c = self.remote_command(None, token);
        c.args(["ls-remote", &url(github)])
            .args(updates.iter().map(|u| u.name.as_str()));
        let out = tokio::time::timeout(Duration::from_secs(30), crate::git::run(&mut c, None))
            .await
            .map_err(|_| "GitHub did not answer in time".to_string())?
            .map_err(|e| e.to_string())?;
        // A pattern also matches longer names that end with it.
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .filter(|(_, name)| updates.iter().any(|u| u.name.as_str() == *name))
            .map(|(oid, name)| (name.to_string(), oid.to_string()))
            .collect())
    }
}

/// An object ID as `git push` takes it; nothing for an absent ref.
fn oid(o: &Option<Oid>) -> &str {
    o.as_ref().map_or("", |o| o.as_str())
}

/// The reasons of a refused `git push --porcelain`: its lines are
/// `<flag>\t<from>:<to>\t<summary> (<reason>)`, with `!` for a refused ref.
fn refusals(updates: &[RefUpdate], stdout: &str, stderr: &str) -> Vec<(String, String)> {
    let remote = remote_errors(stderr);
    let mut refused = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some("!"), Some(refs), Some(summary)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some((_, name)) = refs.rsplit_once(':') else {
            continue;
        };
        // Refs that only fail with the others get the push procedure's
        // reason.
        if summary.contains("atomic") {
            continue;
        }
        let why = if summary.contains("`workflow` scope") {
            WORKFLOW_SCOPE.to_string()
        } else if [
            "stale info",
            "fetch first",
            "non-fast-forward",
            "already exists",
        ]
        .iter()
        .any(|s| summary.contains(s))
        {
            STALE.to_string()
        } else {
            let reason = summary
                .rsplit_once('(')
                .and_then(|(_, r)| r.strip_suffix(')'))
                .unwrap_or(summary);
            format!("GitHub refused it: {}", remote.as_deref().unwrap_or(reason))
        };
        refused.push((name.to_string(), why));
    }
    if refused.is_empty() {
        let why = if stderr.contains("`workflow` scope") {
            WORKFLOW_SCOPE.to_string()
        } else if let Some(r) = remote {
            format!("GitHub refused it: {r}")
        } else {
            remote_message(stderr)
        };
        refused = updates
            .iter()
            .map(|u| (u.name.to_string(), why.clone()))
            .collect();
    }
    refused
}

/// What GitHub said about a refused push, such as the rule of a protected
/// branch: the `remote: error:` lines and the `remote: - …` reasons below
/// them.
fn remote_errors(stderr: &str) -> Option<String> {
    let mut errors = false;
    let mut said = Vec::new();
    for line in stderr.lines() {
        let Some(line) = line.trim().strip_prefix("remote:").map(str::trim) else {
            continue;
        };
        let text = match line.strip_prefix("error:") {
            Some(e) => {
                errors = true;
                e.trim()
            }
            None => match line.strip_prefix("- ") {
                Some(reason) if errors => reason.trim(),
                _ => continue,
            },
        };
        if !text.is_empty() && !text.starts_with("Trace:") && !text.starts_with("See ") {
            said.push(text);
        }
    }
    let text = said.join(" ");
    (!text.is_empty()).then(|| text.chars().take(400).collect())
}
