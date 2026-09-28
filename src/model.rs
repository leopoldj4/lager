//! Identifiers, record formats, ref checks, and transaction rules.
//!
//! Everything in this module is pure: no I/O, no Git processes. The records
//! defined here are the storage format (see ARCHITECTURE.md §5).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The only record format version this build reads and writes.
pub const FORMAT_VERSION: u32 = 1;

macro_rules! uuid_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

uuid_id!(
    /// Identifies a repository. Never derived from a client path.
    RepoId
);
uuid_id!(
    /// Identifies one lifetime of a repository.
    IncarnationId
);
uuid_id!(
    /// Changes on every head update, even when refs do not change.
    RevisionId
);
uuid_id!(EntryId);
uuid_id!(RequestId);
uuid_id!(SnapshotId);

/// Object hash algorithm of a repository. Saved explicitly so that SHA-256
/// can be added later without guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectFormat {
    Sha1,
}

impl ObjectFormat {
    pub fn hex_len(self) -> usize {
        match self {
            ObjectFormat::Sha1 => 40,
        }
    }

    pub fn git_name(self) -> &'static str {
        match self {
            ObjectFormat::Sha1 => "sha1",
        }
    }

    /// Git's all-zero OID, used only at the Git interface.
    pub fn zero_oid(self) -> String {
        "0".repeat(self.hex_len())
    }
}

/// A Git object identifier in lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Oid(String);

impl Oid {
    pub fn parse(format: ObjectFormat, s: &str) -> Result<Oid, ModelError> {
        if s.len() != format.hex_len()
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ModelError::InvalidOid(s.chars().take(80).collect()));
        }
        Ok(Oid(s.to_string()))
    }

    /// Parses a Git-interface OID, mapping the zero OID to `None`.
    pub fn parse_git(format: ObjectFormat, s: &str) -> Result<Option<Oid>, ModelError> {
        let oid = Oid::parse(format, s)?;
        Ok(if oid.0.bytes().all(|b| b == b'0') {
            None
        } else {
            Some(oid)
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Converts an optional OID to Git's representation.
    pub fn to_git(oid: Option<&Oid>, format: ObjectFormat) -> String {
        match oid {
            Some(o) => o.0.clone(),
            None => format.zero_oid(),
        }
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A validated ref name. Only branches and tags are supported.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RefName(String);

impl RefName {
    pub fn parse(s: &str) -> Result<RefName, ModelError> {
        let bad = |why: &str| {
            Err(ModelError::InvalidRef(
                s.chars().take(200).collect(),
                why.into(),
            ))
        };
        if s.len() > 1024 {
            return bad("too long");
        }
        let kind_ok = s.starts_with("refs/heads/") || s.starts_with("refs/tags/");
        if !kind_ok {
            return bad("only refs/heads/ and refs/tags/ are supported");
        }
        if s.ends_with('/')
            || s.ends_with('.')
            || s.contains("..")
            || s.contains("@{")
            || s.contains("//")
        {
            return bad("invalid ref name");
        }
        for b in s.bytes() {
            if b < 0x20 || b == 0x7f || b" ~^:?*[\\".contains(&b) {
                return bad("invalid character in ref name");
            }
        }
        for comp in s.split('/') {
            if comp.is_empty() || comp.starts_with('.') || comp.ends_with(".lock") || comp == "@" {
                return bad("invalid ref name component");
            }
        }
        Ok(RefName(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_branch(&self) -> bool {
        self.0.starts_with("refs/heads/")
    }

    pub fn is_tag(&self) -> bool {
        self.0.starts_with("refs/tags/")
    }
}

impl TryFrom<String> for RefName {
    type Error = ModelError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        RefName::parse(&s)
    }
}

impl From<RefName> for String {
    fn from(r: RefName) -> String {
        r.0
    }
}

impl fmt::Display for RefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validates a branch short name (for the default branch).
pub fn branch_ref(short: &str) -> Result<RefName, ModelError> {
    RefName::parse(&format!("refs/heads/{short}"))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("invalid object id {0:?}")]
    InvalidOid(String),
    #[error("invalid ref {0:?}: {1}")]
    InvalidRef(String, String),
    #[error("unsupported record format {0}")]
    UnsupportedFormat(u32),
    #[error("record belongs to another repository or lifetime")]
    WrongRepository,
}

/// One requested ref change. `None` means the ref is absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefUpdate {
    pub name: RefName,
    pub old: Option<Oid>,
    pub new: Option<Oid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoState {
    Active,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPointer {
    pub id: SnapshotId,
    pub base_entry: EntryId,
    pub base_generation: u64,
}

/// `repos/<id>/head.json`. The only record that changes in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub format: u32,
    pub repository: RepoId,
    pub incarnation: IncarnationId,
    pub revision: RevisionId,
    pub generation: u64,
    pub latest_entry: EntryId,
    pub snapshot: Option<SnapshotPointer>,
    pub object_format: ObjectFormat,
    pub state: RepoState,
}

impl Head {
    /// The head that follows `self` after appending `entry`.
    pub fn advance(&self, entry: &LogEntry) -> Head {
        let mut next = self.clone();
        next.revision = RevisionId::new();
        next.generation = entry.generation;
        next.latest_entry = entry.id;
        match &entry.operation {
            Operation::Snapshot {
                snapshot,
                base_entry,
                base_generation,
            } => {
                next.snapshot = Some(SnapshotPointer {
                    id: *snapshot,
                    base_entry: *base_entry,
                    base_generation: *base_generation,
                });
            }
            Operation::Delete => next.state = RepoState::Deleted,
            _ => {}
        }
        next
    }

    pub fn check(&self, repo: RepoId) -> Result<(), ModelError> {
        if self.format != FORMAT_VERSION {
            return Err(ModelError::UnsupportedFormat(self.format));
        }
        if self.repository != repo {
            return Err(ModelError::WrongRepository);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Operation {
    /// First entry of an incarnation.
    Create {
        default_branch: RefName,
    },
    Push,
    Snapshot {
        snapshot: SnapshotId,
        base_entry: EntryId,
        base_generation: u64,
    },
    DefaultBranch {
        branch: RefName,
    },
    Delete,
}

/// A stored pack. `sha256` is also the storage key; never trust an ETag as a
/// checksum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackRef {
    pub sha256: String,
    pub size: u64,
}

/// `repos/<id>/entries/<entry-id>.json`. Immutable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogEntry {
    pub format: u32,
    pub repository: RepoId,
    pub incarnation: IncarnationId,
    pub id: EntryId,
    pub previous: Option<EntryId>,
    pub generation: u64,
    pub request: RequestId,
    pub request_checksum: String,
    pub operation: Operation,
    pub refs: Vec<RefUpdate>,
    pub packs: Vec<PackRef>,
    pub user: String,
    pub rules_version: u64,
    /// Milliseconds since the Unix epoch. For history only, never for order.
    pub received_at_ms: u64,
}

impl LogEntry {
    pub fn check(&self, repo: RepoId, incarnation: IncarnationId) -> Result<(), ModelError> {
        if self.format != FORMAT_VERSION {
            return Err(ModelError::UnsupportedFormat(self.format));
        }
        if self.repository != repo || self.incarnation != incarnation {
            return Err(ModelError::WrongRepository);
        }
        Ok(())
    }
}

/// `repos/<id>/snapshots/<snapshot-id>.json`. Immutable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub format: u32,
    pub repository: RepoId,
    pub incarnation: IncarnationId,
    pub id: SnapshotId,
    pub base_entry: EntryId,
    pub base_generation: u64,
    pub object_format: ObjectFormat,
    /// Target of Git `HEAD`.
    pub head: RefName,
    pub refs: BTreeMap<RefName, Oid>,
    pub packs: Vec<PackRef>,
}

/// `repos/<id>/receipts/<request-id>.json`. Immutable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub format: u32,
    pub repository: RepoId,
    pub incarnation: IncarnationId,
    pub request: RequestId,
    pub request_checksum: String,
    pub entry: EntryId,
    pub generation: u64,
}

/// Checksum of the request contents. Internal retries reuse the request UUID
/// only with identical contents.
pub fn request_checksum(user: &str, updates: &[RefUpdate]) -> String {
    let mut h = Sha256::new();
    h.update(user.as_bytes());
    h.update([0]);
    for u in updates {
        h.update(u.name.as_str().as_bytes());
        h.update([0]);
        h.update(u.old.as_ref().map(|o| o.as_str()).unwrap_or("-").as_bytes());
        h.update([0]);
        h.update(u.new.as_ref().map(|o| o.as_str()).unwrap_or("-").as_bytes());
        h.update([0]);
    }
    hex::encode(h.finalize())
}

/// Refs and Git `HEAD` of one accepted generation.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RefState {
    pub refs: BTreeMap<RefName, Oid>,
    pub head: Option<RefName>,
}

/// Why a single requested ref change was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub name: String,
    pub reason: String,
}

impl RefState {
    /// Applies the ref changes and default-branch changes of an accepted entry.
    /// Fails when the entry does not fit this state, which means the local
    /// state is damaged or the history is broken.
    pub fn apply_entry(&mut self, entry: &LogEntry) -> Result<(), String> {
        match &entry.operation {
            Operation::Create { default_branch } => {
                self.refs.clear();
                self.head = Some(default_branch.clone());
            }
            Operation::DefaultBranch { branch } => self.head = Some(branch.clone()),
            Operation::Push | Operation::Snapshot { .. } | Operation::Delete => {}
        }
        for u in &entry.refs {
            if self.refs.get(&u.name) != u.old.as_ref() {
                return Err(format!(
                    "entry {} does not apply: ref {} differs",
                    entry.id, u.name
                ));
            }
            match &u.new {
                Some(n) => self.refs.insert(u.name.clone(), n.clone()),
                None => self.refs.remove(&u.name),
            };
        }
        Ok(())
    }

    /// Checks requested changes against this state with the transaction rules.
    /// Returns the resulting state, or one rejection per failing ref.
    /// All-or-nothing: any rejection rejects the complete request.
    pub fn check_updates(&self, updates: &[RefUpdate]) -> Result<RefState, Vec<Rejection>> {
        let mut rejections = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for u in updates {
            if !seen.insert(&u.name) {
                rejections.push(Rejection {
                    name: u.name.to_string(),
                    reason: "duplicate ref command".into(),
                });
                continue;
            }
            if u.old.is_none() && u.new.is_none() {
                rejections.push(Rejection {
                    name: u.name.to_string(),
                    reason: "deletion of a missing ref".into(),
                });
                continue;
            }
            if self.refs.get(&u.name) != u.old.as_ref() {
                let reason = match (self.refs.get(&u.name), &u.old) {
                    (None, Some(_)) => "ref does not exist (fetch first)",
                    (Some(_), None) => "ref already exists (fetch first)",
                    _ => "stale info: ref changed (fetch first)",
                };
                rejections.push(Rejection {
                    name: u.name.to_string(),
                    reason: reason.into(),
                });
            }
        }
        if !rejections.is_empty() {
            return Err(rejections);
        }
        let mut next = self.clone();
        for u in updates {
            match &u.new {
                Some(n) => next.refs.insert(u.name.clone(), n.clone()),
                None => next.refs.remove(&u.name),
            };
        }
        // Name conflicts such as refs/heads/x with refs/heads/x/y.
        for u in updates.iter().filter(|u| u.new.is_some()) {
            if let Some(other) = next.conflicting_name(&u.name) {
                rejections.push(Rejection {
                    name: u.name.to_string(),
                    reason: format!("name conflicts with {other}"),
                });
            }
        }
        if rejections.is_empty() {
            Ok(next)
        } else {
            Err(rejections)
        }
    }

    fn conflicting_name(&self, name: &RefName) -> Option<&RefName> {
        let n = name.as_str();
        // A shorter ref that is a directory prefix of `name`.
        let mut idx = 0;
        while let Some(pos) = n[idx..].find('/') {
            let prefix = &n[..idx + pos];
            if let Some((k, _)) = self.refs.get_key_value(&RefName(prefix.to_string())) {
                return Some(k);
            }
            idx += pos + 1;
        }
        // A longer ref under `name/`.
        let dir = format!("{n}/");
        self.refs
            .range(RefName(dir.clone())..)
            .next()
            .map(|(k, _)| k)
            .filter(|k| k.as_str().starts_with(&dir))
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(c: char) -> Oid {
        Oid::parse(ObjectFormat::Sha1, &c.to_string().repeat(40)).unwrap()
    }
    fn r(s: &str) -> RefName {
        RefName::parse(s).unwrap()
    }
    fn upd(name: &str, old: Option<char>, new: Option<char>) -> RefUpdate {
        RefUpdate {
            name: r(name),
            old: old.map(oid),
            new: new.map(oid),
        }
    }

    #[test]
    fn ref_names_follow_git_rules_and_only_allow_branches_and_tags() {
        for ok in [
            "refs/heads/main",
            "refs/heads/feature/x-1",
            "refs/tags/v1.0",
        ] {
            assert!(RefName::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "HEAD",
            "refs/notes/x",
            "refs/heads/",
            "refs/heads/a..b",
            "refs/heads/.hidden",
            "refs/heads/x.lock",
            "refs/heads/a b",
            "refs/heads/a~1",
            "refs/heads/a:b",
            "refs/heads/a@{1}",
            "refs/heads//a",
            "refs/heads/a.",
            "refs/heads/a\\b",
            "refs/heads/@",
        ] {
            assert!(RefName::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn zero_oid_maps_to_absent_at_the_git_interface() {
        let z = ObjectFormat::Sha1.zero_oid();
        assert_eq!(Oid::parse_git(ObjectFormat::Sha1, &z).unwrap(), None);
        assert_eq!(Oid::to_git(None, ObjectFormat::Sha1), z);
        assert!(
            Oid::parse(
                ObjectFormat::Sha1,
                "ABCDEF0123456789abcdef0123456789abcdef01"
            )
            .is_err()
        );
        assert!(Oid::parse(ObjectFormat::Sha1, "abc").is_err());
    }

    #[test]
    fn stale_old_value_rejects_the_request() {
        let mut s = RefState::default();
        s.refs.insert(r("refs/heads/main"), oid('a'));
        let err = s
            .check_updates(&[upd("refs/heads/main", Some('b'), Some('c'))])
            .unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].reason.contains("stale"));
        let err = s
            .check_updates(&[upd("refs/heads/main", None, Some('c'))])
            .unwrap_err();
        assert!(err[0].reason.contains("already exists"));
    }

    #[test]
    fn two_changes_from_the_same_old_value_cannot_both_apply() {
        let mut s = RefState::default();
        s.refs.insert(r("refs/heads/main"), oid('a'));
        let first = s
            .check_updates(&[upd("refs/heads/main", Some('a'), Some('b'))])
            .unwrap();
        assert!(
            first
                .check_updates(&[upd("refs/heads/main", Some('a'), Some('c'))])
                .is_err()
        );
    }

    #[test]
    fn changes_to_different_refs_both_apply() {
        let s = RefState::default();
        let s1 = s
            .check_updates(&[upd("refs/heads/a", None, Some('a'))])
            .unwrap();
        let s2 = s1
            .check_updates(&[upd("refs/heads/b", None, Some('b'))])
            .unwrap();
        assert_eq!(s2.refs.len(), 2);
    }

    #[test]
    fn directory_file_name_conflicts_are_rejected() {
        let mut s = RefState::default();
        s.refs.insert(r("refs/heads/x"), oid('a'));
        let err = s
            .check_updates(&[upd("refs/heads/x/y", None, Some('b'))])
            .unwrap_err();
        assert!(err[0].reason.contains("refs/heads/x"));
        let mut s = RefState::default();
        s.refs.insert(r("refs/heads/x/y"), oid('a'));
        assert!(
            s.check_updates(&[upd("refs/heads/x", None, Some('b'))])
                .is_err()
        );
        // Deleting x in the same transaction permits x/y.
        let mut s = RefState::default();
        s.refs.insert(r("refs/heads/x"), oid('a'));
        let ok = s
            .check_updates(&[
                upd("refs/heads/x", Some('a'), None),
                upd("refs/heads/x/y", None, Some('b')),
            ])
            .unwrap();
        assert_eq!(
            ok.refs.keys().map(|k| k.as_str()).collect::<Vec<_>>(),
            vec!["refs/heads/x/y"]
        );
        // Siblings with a common prefix are not conflicts.
        let mut s = RefState::default();
        s.refs.insert(r("refs/heads/x"), oid('a'));
        assert!(
            s.check_updates(&[upd("refs/heads/xy", None, Some('b'))])
                .is_ok()
        );
    }

    #[test]
    fn duplicate_commands_and_missing_deletions_are_rejected() {
        let s = RefState::default();
        let err = s
            .check_updates(&[
                upd("refs/heads/a", None, Some('a')),
                upd("refs/heads/a", None, Some('b')),
            ])
            .unwrap_err();
        assert!(err[0].reason.contains("duplicate"));
        assert!(s.check_updates(&[upd("refs/heads/a", None, None)]).is_err());
    }

    #[test]
    fn records_with_unknown_format_are_rejected() {
        let head = Head {
            format: 2,
            repository: RepoId::new(),
            incarnation: IncarnationId::new(),
            revision: RevisionId::new(),
            generation: 0,
            latest_entry: EntryId::new(),
            snapshot: None,
            object_format: ObjectFormat::Sha1,
            state: RepoState::Active,
        };
        assert_eq!(
            head.check(head.repository),
            Err(ModelError::UnsupportedFormat(2))
        );
        let json = serde_json::to_string(&head)
            .unwrap()
            .replace("\"state\"", "\"extra\":1,\"state\"");
        assert!(serde_json::from_str::<Head>(&json).is_err());
    }

    #[test]
    fn request_checksum_depends_on_contents() {
        let a = request_checksum("u", &[upd("refs/heads/a", None, Some('a'))]);
        let b = request_checksum("u", &[upd("refs/heads/a", None, Some('b'))]);
        assert_ne!(a, b);
        assert_eq!(
            a,
            request_checksum("u", &[upd("refs/heads/a", None, Some('a'))])
        );
    }
}
