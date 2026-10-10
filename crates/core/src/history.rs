//! Reading a repository's history: the snapshot and log entries that the
//! head's index names. Accepted changes are found only through the index,
//! never by listing storage.

use futures::{StreamExt, TryStreamExt};

use crate::model::{EntryId, Head, LogEntry, RefState, Snapshot};
use crate::storage::{Storage, StorageError};

/// The records needed to reconstruct one head.
pub struct Chain {
    pub snapshot: Option<Snapshot>,
    /// Entries after the snapshot base (or from the first entry), oldest first.
    pub entries: Vec<LogEntry>,
}

/// Entries read from storage at the same time.
const PARALLEL_READS: usize = 16;

/// Loads the indexed entries from generation `from` to the head's generation,
/// oldest first, and checks each against its place in the index. `from` must
/// not precede the index.
pub async fn load_entries(
    storage: &Storage,
    head: &Head,
    from: u64,
) -> Result<Vec<LogEntry>, StorageError> {
    let first = head.first_generation();
    let skip = from.checked_sub(first).ok_or_else(|| {
        StorageError::Corrupt(format!(
            "generation {from} precedes the index (from {first})"
        ))
    })?;
    let wanted: Vec<(u64, EntryId)> = head
        .entries
        .iter()
        .copied()
        .enumerate()
        .skip(skip as usize)
        .map(|(i, id)| (first + i as u64, id))
        .collect();
    futures::stream::iter(wanted)
        .map(|(generation, id)| async move {
            let e = storage.get_entry(head.repository, id).await?;
            e.check(head.repository, head.incarnation)
                .map_err(|m| StorageError::Corrupt(format!("entry {id}: {m}")))?;
            if e.generation != generation {
                return Err(StorageError::Corrupt(format!(
                    "entry {id}: generation {} != {generation}",
                    e.generation
                )));
            }
            Ok(e)
        })
        .buffered(PARALLEL_READS)
        .try_collect()
        .await
}

pub async fn load_chain(storage: &Storage, head: &Head) -> Result<Chain, StorageError> {
    let entries = load_entries(storage, head, head.first_generation()).await?;
    let snapshot = match &head.snapshot {
        Some(p) => {
            let s = storage.get_snapshot(head.repository, p.id).await?;
            if s.base_entry != p.base_entry
                || s.base_generation != p.base_generation
                || s.incarnation != head.incarnation
            {
                return Err(StorageError::Corrupt(format!(
                    "snapshot {} does not match the head",
                    p.id
                )));
            }
            Some(s)
        }
        None => None,
    };
    if snapshot.is_none() && entries.first().map(|e| e.generation) != Some(0) {
        return Err(StorageError::Corrupt(
            "history does not start with a creation entry".into(),
        ));
    }
    Ok(Chain { snapshot, entries })
}

/// The ref state that a chain produces.
pub fn chain_state(chain: &Chain) -> Result<RefState, String> {
    let mut state = RefState::default();
    if let Some(s) = &chain.snapshot {
        state.refs = s.refs.clone();
        state.head = Some(s.head.clone());
    }
    for e in &chain.entries {
        state.apply_entry(e)?;
    }
    Ok(state)
}
