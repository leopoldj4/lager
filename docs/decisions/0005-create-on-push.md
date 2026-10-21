# 5. Repositories created by a push

Status: accepted. Date: September 29, 2026.
`catalog.create_on_push` is fixed at `write` by [decision 15](0015-minimal-configuration.md).

## Context

With the storage catalog, a repository had to be created with
`lager repo create` before the first push, which needs storage keys on
the operator's computer.

## Decision

**Creation.** With `catalog.kind = "storage"` and `catalog.create_on_push`
set to an access level, a receive-pack request (`info/refs` or the push
itself) for a missing name creates the repository with default settings,
as `lager repo create` would. The user must be authenticated and have at
least that level, and at least write, for the name: a grant for the name
itself or `*`. Anonymous clients are asked for credentials first; reads
never create. Both requests of a push may create, because a server whose
lookup is older than `catalog.cache_secs` does not yet know a repository
that another server created; creation reads the record first and returns
the existing repository. Concurrent creations have one winner. Creation is
logged and counted (`lager_repositories_created_on_push`).

**Default branch.** A push accepted into an empty repository that does not
create the default branch makes one of the pushed branches the default:
`main`, then `master`, then the first by name. This is a second log entry
(`default-branch`) after the push, and the catalog record follows. It
applies to every empty repository, not only to created ones, so that
clones check out what was pushed.

## Consequences

- A mistyped URL in `git push` creates a repository, as does
  `git push --dry-run` and a push that is rejected later. Delete it with
  `lager repo delete`.
- If the default-branch change fails, the push still counts; the server
  logs a warning and `lager repo set-default-branch` fixes it.
