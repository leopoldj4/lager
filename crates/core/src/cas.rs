//! Head replacements whose result is unknown.

use std::time::{Duration, Instant};

use crate::model::{EntryId, Head};
use crate::storage::{CasOutcome, ETag, Storage};

/// Result of a CAS whose first answer was lost.
#[derive(Debug)]
pub enum Resolution {
    /// The head contains the change. The ETag is known if our write is still
    /// the current head.
    Accepted(Option<ETag>),
    /// A later head excludes the change; the earlier CAS cannot succeed.
    Rejected,
    /// The result is still unknown.
    Unknown(String),
}

/// Determines the result of a CAS with an unknown result: repeat the
/// identical CAS, then look for the entry in the retained history.
pub async fn resolve_unknown_cas(
    storage: &Storage,
    new_head: &Head,
    expected: &ETag,
    entry: EntryId,
    deadline: Instant,
) -> Resolution {
    let mut last = String::from("no answer");
    let mut attempt = 0u32;
    loop {
        if Instant::now() >= deadline {
            return Resolution::Unknown(last);
        }
        match storage.replace_head(new_head, expected).await {
            CasOutcome::Accepted(etag) => return Resolution::Accepted(Some(etag)),
            CasOutcome::Conflict => match find_in_history(storage, new_head, entry).await {
                Ok(r) => return r,
                Err(e) => last = e,
            },
            CasOutcome::Unknown(m) => last = m,
        }
        attempt += 1;
        tokio::time::sleep(jitter(attempt)).await;
    }
}

/// After a definite conflict: did an earlier attempt of ours succeed? The
/// current head's index answers this until a snapshot folds the attempted
/// generation; the result then stays unknown.
async fn find_in_history(
    storage: &Storage,
    new_head: &Head,
    entry: EntryId,
) -> Result<Resolution, String> {
    let (head, etag) = match storage.load_head(new_head.repository).await {
        Ok(Some(h)) => h,
        Ok(None) => return Ok(Resolution::Rejected),
        Err(e) => return Err(e.to_string()),
    };
    if head.revision == new_head.revision {
        return Ok(Resolution::Accepted(Some(etag)));
    }
    if head.incarnation != new_head.incarnation || head.generation < new_head.generation {
        return Ok(Resolution::Rejected);
    }
    if head.contains(new_head.generation, entry) {
        return Ok(Resolution::Accepted(None));
    }
    if head.entry_at(new_head.generation).is_some()
        || head
            .snapshot
            .as_ref()
            .is_some_and(|s| s.base_generation == new_head.generation)
    {
        return Ok(Resolution::Rejected);
    }
    Err("a snapshot folded the attempted generation".into())
}

/// Randomized backoff before retry `attempt`.
pub fn jitter(attempt: u32) -> Duration {
    let base = 20u64 << attempt.min(6);
    Duration::from_millis(base + rand::random::<u64>() % base)
}
