# 2. Protect active operations with a fence before data removal

Status: accepted. Date: September 27, 2026.

## Context

The design (ARCHITECTURE.md §9) requires that data removal cannot break
active reads, active writes, or requests with unknown results, and that
"object age alone does not prove that deletion is safe". It also requires
that expired operations cannot later accept changes that need deleted data.

Age alone fails in two cases:

1. A CAS request is delayed in the network or in the storage service. The
   repository stays idle, so the head keeps its old ETag. Cleanup removes the
   request's pack because it is old and unreferenced. Then the delayed CAS
   arrives and succeeds: the head now names a removed pack.
2. Packs are content addressed. A push can upload a pack with the same
   checksum as an old obsolete pack just before cleanup removes that key.

## Decision

Data removal (`rustgit cleanup --delete`) runs in three steps:

1. **Plan.** Read the head and find the obsolete objects: superseded
   snapshots, packs that only entries at or before the snapshot base use,
   uploads of attempts that never became visible, and old receipts. Objects
   younger than their retention period are kept. Packs and records have
   separate retention periods.
2. **Fence.** Attach a `removal` log entry with CAS. The entry lists the
   packs to remove, the end of the removal window (`expires_at_ms`), and
   the time after which listed packs can be used again (`reuse_after_ms`,
   the window plus the allowed clock difference between servers). The head
   keeps a pointer to the latest fence.
3. **Remove.** Delete the listed objects, and stop when the window ends.

The fence gives these guarantees:

- Every CAS against an earlier head fails. A delayed or unknown request
  cannot succeed after the fence, so it cannot accept a change that needs
  removed data (case 1).
- A push or snapshot always reads the head before it uploads its pack, and
  checks the fence before it attaches an entry. If one of its packs is on
  the removal list and was uploaded before `reuse_after_ms`, the change
  waits (the client gets "retry later") or, after that time, uploads the pack
  again (case 2).
- Only one fence is active: cleanup refuses to start while the latest fence
  is active. Earlier fences expired before the latest one was attached, so a
  change needs to check only the latest one.
- Recovery never needs removed data: the head's snapshot and the entries
  after its base name only required packs.

The retention period still matters: with a retention longer than the push
deadline and the unknown-result deadline, active pushes never meet the fence
at all. The command refuses shorter periods.

## Format change

This adds the `removal` operation type to log entries and an optional
`removal` pointer to the head. Heads without a fence serialize as before.
The record format version stays 1: there is no deployed data.
