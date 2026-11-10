//! A user's profile: their activity of the last year, here and on GitHub
//! (docs/decisions/0029-activity-graph.md). Anyone may read it.
//!
//! The activity here counts commits with one of the user's email addresses
//! on the default branch of each repository the user can read, private
//! ones included, and the pull requests they opened; only the counts per
//! day leave the server. With a GitHub account whose counts were fetched,
//! GitHub's counts join them, and mirrors leave the counts here: GitHub
//! counts their commits already.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::read::ReadError;
use crate::http::AppInner;
use crate::repository::RepoError;
use lager_core::model::RepoState;
use lager_core::repo::Access;

pub async fn profile(s: &AppInner, user: &str) -> Result<Value, ReadError> {
    if lager_core::identity::validate_user_name(user).is_err() {
        return Err(ReadError::not_found("user"));
    }
    let owner = match s.directory.lookup(user).await {
        Ok(Some(u)) => u.identity,
        Ok(None) => return Err(ReadError::not_found("user")),
        Err(e) => {
            tracing::warn!(error = ?e, "user store unavailable");
            return Err(ReadError::new(
                "unavailable",
                "users unavailable; retry later",
            ));
        }
    };
    let github = match &s.github {
        Some(g) => g.contributions(user).await.unwrap_or_else(|e| {
            tracing::warn!(user, error = %e, "GitHub contributions unavailable");
            None
        }),
        None => None,
    };
    let repos: Vec<_> = s
        .directory
        .catalog()
        .list()
        .await
        .map_err(|e| ReadError::from(RepoError::from(e)))?
        .into_iter()
        .filter(|r| r.state == RepoState::Active)
        .map(|r| r.settings())
        .filter(|r| s.directory.access(&owner, r) >= Access::Read)
        .filter(|r| github.is_none() || r.mirror.is_none())
        .map(|r| r.id)
        .collect();
    let here = s.activity.db.days(user, &repos).await.map_err(|e| {
        tracing::warn!(error = %e, "activity unavailable");
        ReadError::new("unavailable", "activity unavailable; retry later")
    })?;

    let mut days: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for d in &here {
        days.entry(d.date.clone()).or_default().0 += d.count;
    }
    for d in github.iter().flatten() {
        days.entry(d.date.clone()).or_default().1 += d.count;
    }
    let total = |f: fn(&(u32, u32)) -> u32| days.values().map(f).sum::<u32>();
    Ok(json!({
        "user": user,
        "github": github.is_some(),
        "totals": { "lager": total(|d| d.0), "github": total(|d| d.1) },
        "days": days
            .iter()
            .map(|(date, (lager, github))| json!({ "date": date, "lager": lager, "github": github }))
            .collect::<Vec<_>>(),
    }))
}
