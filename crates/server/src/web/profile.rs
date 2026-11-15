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

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ops::{Op, OpError};
use crate::http::AppInner;
use crate::repository::RepoError;
use lager_core::identity::Identity;
use lager_core::model::RepoState;
use lager_core::repo::Access;

/// A day with activity.
#[derive(Serialize, TS)]
pub struct ActivityDay {
    /// `YYYY-MM-DD`.
    date: String,
    lager: u32,
    github: u32,
}

#[derive(Default, Serialize, TS)]
pub struct ActivityTotals {
    lager: u32,
    github: u32,
}

#[derive(Serialize, TS)]
pub struct Profile {
    user: String,
    /// GitHub's counts are included; mirrors count only there.
    github: bool,
    totals: ActivityTotals,
    /// Days with activity in the last year, oldest first.
    days: Vec<ActivityDay>,
}

/// A user's activity of the last year; anyone may read it.
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ProfileOp {
    user: String,
}

impl Op for ProfileOp {
    type Answer = Profile;

    async fn run(self, s: &AppInner, _: &Identity) -> Result<Profile, OpError> {
        profile(s, &self.user).await
    }
}

async fn profile(s: &AppInner, user: &str) -> Result<Profile, OpError> {
    if lager_core::identity::validate_user_name(user).is_err() {
        return Err(OpError::not_found("user"));
    }
    let owner = match s.directory.lookup(user).await {
        Ok(Some(u)) => u.identity,
        Ok(None) => return Err(OpError::not_found("user")),
        Err(e) => {
            tracing::warn!(error = ?e, "user store unavailable");
            return Err(OpError::new(
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
        .map_err(|e| OpError::from(RepoError::from(e)))?
        .into_iter()
        .filter(|r| r.state == RepoState::Active)
        .map(|r| r.settings())
        .filter(|r| s.directory.access(&owner, r) >= Access::Read)
        .filter(|r| github.is_none() || r.mirror.is_none())
        .map(|r| r.id)
        .collect();
    let here = s.activity.db.days(user, &repos).await.map_err(|e| {
        tracing::warn!(error = %e, "activity unavailable");
        OpError::new("unavailable", "activity unavailable; retry later")
    })?;

    let mut days: BTreeMap<&str, ActivityTotals> = BTreeMap::new();
    for d in &here {
        days.entry(&d.date).or_default().lager += d.count;
    }
    for d in github.iter().flatten() {
        days.entry(&d.date).or_default().github += d.count;
    }
    let days: Vec<ActivityDay> = days
        .into_iter()
        .map(|(date, ActivityTotals { lager, github })| ActivityDay {
            date: date.to_string(),
            lager,
            github,
        })
        .collect();
    Ok(Profile {
        user: user.to_string(),
        github: github.is_some(),
        totals: ActivityTotals {
            lager: days.iter().map(|d| d.lager).sum(),
            github: days.iter().map(|d| d.github).sum(),
        },
        days,
    })
}
