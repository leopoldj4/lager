//! Repository settings, access levels, and ref rules. The settings live in
//! the repository catalog in storage ([`crate::catalog`]); access levels of
//! users live in the database ([`crate::identity`]).

use serde::{Deserialize, Serialize};

use crate::model::RepoId;

/// The settings of one repository, from its catalog record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSettings {
    pub name: String,
    pub id: RepoId,
    pub default_branch: String,
    pub anonymous_read: bool,
    pub ref_rules: Vec<RefRule>,
    /// The GitHub repository (`owner/name`) that this repository is a
    /// read-only mirror of (docs/decisions/0020-github-mirrors.md).
    pub mirror: Option<String>,
}

impl RepoSettings {
    pub fn new(name: &str, id: RepoId) -> Self {
        Self {
            name: name.into(),
            id,
            default_branch: "main".into(),
            anonymous_read: false,
            ref_rules: vec![],
            mirror: None,
        }
    }
}

/// The access levels that changes to matching refs need.
///
/// Without a matching rule, creation, fast-forward updates, and deletion
/// need `write`; non-fast-forward updates and tag moves need `force`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RefRule {
    /// A complete ref name, or a prefix that ends with `*`.
    pub pattern: String,
    /// Creation and fast-forward updates.
    #[serde(default = "access_write")]
    pub update: Access,
    /// Non-fast-forward updates and tag moves.
    #[serde(default = "access_force")]
    pub force: Access,
    #[serde(default = "access_write")]
    pub delete: Access,
}

impl RefRule {
    /// A rule with the levels that apply without one.
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.into(),
            update: Access::Write,
            force: Access::Force,
            delete: Access::Write,
        }
    }

    fn matches(&self, name: &str) -> bool {
        match self.pattern.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == self.pattern,
        }
    }
}

pub fn validate_ref_rule(rule: &RefRule) -> anyhow::Result<()> {
    let body = rule.pattern.strip_suffix('*').unwrap_or(&rule.pattern);
    anyhow::ensure!(
        body.starts_with("refs/") && !body.contains('*'),
        "invalid ref rule pattern {:?}: use a ref name or a prefix ending in *",
        rule.pattern
    );
    Ok(())
}

/// The most specific rule for a ref: an exact name, else the longest prefix.
pub fn ref_rule<'a>(rules: &'a [RefRule], name: &str) -> Option<&'a RefRule> {
    rules
        .iter()
        .filter(|r| r.matches(name))
        .max_by_key(|r| (!r.pattern.ends_with('*'), r.pattern.len()))
}

fn access_write() -> Access {
    Access::Write
}
fn access_force() -> Access {
    Access::Force
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    None,
    Read,
    Write,
    /// Write plus force pushes (non-fast-forward branch updates, tag moves).
    Force,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::None => "none",
            Access::Read => "read",
            Access::Write => "write",
            Access::Force => "force",
        }
    }
}

impl std::str::FromStr for Access {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "none" => Access::None,
            "read" => Access::Read,
            "write" => Access::Write,
            "force" => Access::Force,
            _ => {
                return Err(format!(
                    "unknown access level {s:?}: use none, read, write, or force"
                ));
            }
        })
    }
}

/// Repository names: segments of `[A-Za-z0-9._-]`, separated by `/`, never
/// starting with `.`. The first segment is the owning user's name and at
/// least one more follows (`alice/app`, `alice/tools/app`). A segment `-` is
/// reserved for the web interface (`/-/api/`, `/<name>/-/tree/...`). The name
/// is never used as a disk path or storage key.
pub fn validate_repo_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty() && name.len() <= 200,
        "invalid repository name"
    );
    let owner = name.split_once('/').map(|(owner, _)| owner);
    anyhow::ensure!(
        owner.is_some_and(|o| crate::identity::validate_user_name(o).is_ok()),
        "invalid repository name {name:?}: use <user>/<name>, such as alice/app"
    );
    for seg in name.split('/') {
        anyhow::ensure!(
            !seg.is_empty()
                && seg != "-"
                && !seg.starts_with('.')
                && !seg.ends_with(".git")
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid repository name {name:?}"
        );
    }
    Ok(())
}
