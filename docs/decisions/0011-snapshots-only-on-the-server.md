# 11. Snapshots only on the server

Status: accepted. Date: October 10, 2026.

## Context

The server and the administration command line are separate crates
(`worktree-server` and `worktree-cli`) with a shared `worktree-core`. The
command line should not contain server code. `worktree snapshot` was the
only command that needed the repository engine: it built a local Git cache
at the storage head to create the combined pack, so the controllers,
caches, push procedure, and Git wrapper had to be in the shared crate.

## Decision

- `worktree snapshot` is removed. Servers create snapshots automatically
  (`[maintenance] snapshot_after_entries`), on the preferred server of each
  repository.
- The Git wrapper, the repository engine, push handling, snapshots,
  metrics, and disk reservations are in `worktree-server`.
- `worktree-core` keeps what the command line needs: configuration, the
  record formats, storage, the catalog, identities, repository creation and
  deletion (`admin`), reading a repository's history (`history`), resolving
  head replacements with an unknown result (`cas`), data removal
  (`cleanup`), and backup.

## Consequences

- A snapshot cannot be forced by hand, for example before `worktree
  cleanup`. With `snapshot_after_entries = 0`, no snapshots are created.
- The command line no longer runs Git; it needs only the configuration and
  access to storage and the database.
