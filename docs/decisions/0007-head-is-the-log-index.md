# 7. The head is the index of the log

Status: accepted. Date: October 9, 2026.

## Context

Until now each log entry named its previous entry, and the head named only
the latest entry. Finding the accepted changes meant following those links
one storage read at a time. A cache that was many generations behind, a
rebuild, a backup, and data removal each walked the chain entry by entry.

A write-ahead log in object storage can be ordered with an index object:
the index lists the entries, and a conditional write of the index accepts
a change.

## Decision

The head is the index of the log (record format 2 for heads and entries):

- The head lists the entries after the snapshot base, oldest first
  (`entries`). Without a snapshot it lists every entry. The last one is the
  latest entry. Only the index orders entries.
- Log entries no longer name a previous entry. An entry's generation must
  match its place in the index.
- A push, snapshot, default-branch change, deletion, or removal fence still
  uploads its entry first and then appends it to the index with CAS on the
  head. The CAS is still the only step that makes a change visible.
- Attaching a snapshot removes the entries up to its base from the index.
  Those entries are no longer part of the history. Data removal can delete
  them after the record retention period, like entries of attempts that
  never became visible. The entry of the latest removal fence is kept while
  the head points to it.
- A snapshot whose base is older than the attached snapshot is refused: the
  index no longer reaches its base.

Effects:

- A cache that is behind reads the missing entries in parallel from the
  index, instead of one link at a time.
- A cache whose generation a snapshot folded out of the index rebuilds from
  the snapshot. Before, it could still apply the entries after its
  generation.
- The head grows with the entries since the last snapshot, and every CAS
  writes the whole index. Automatic snapshots now run by default
  (`snapshot_after_entries = 200`, before 0) to keep it short.
- The operation history before a snapshot is not kept. A request with an
  unknown result whose generation a snapshot folded during its resolution
  stays unknown, and the client is told to fetch and check.

## Format change

Heads and log entries change to format 2 (`LOG_FORMAT_VERSION`): the head
replaces `latest_entry` with `entries`, and entries drop `previous`. Other
records keep format 1. Format 1 heads are refused: there is no deployed data
to keep.
