# 3. Repository catalog, identity checks, and ref permissions

Status: accepted. Date: September 27, 2026.

## Context

The design keeps the name-to-UUID table in a configuration file at first
and requires a repository catalog, an identity service, and ref permissions
before production use (ARCHITECTURE.md §5, §11; IMPLEMENTATION_PLAN.md
Stage 3 item 4).

## Decision

**Catalog.** With `catalog.kind = "storage"`, repository names come from
`<prefix>catalog/<sha256 of the name>.json` in the storage service, outside
`repos/`. The key is a digest, so a client path never becomes a storage key.
A record holds the name, the current repository UUID, its state, and its
settings (default branch, anonymous reads, customer, ref rules). It follows
the rules of the head: conditional creation, then CAS only. Deletion marks
the repository head as deleted first and the record second. A reused name
gets a new repository UUID. Lookups are reused for `catalog.cache_secs`;
because every request also checks the repository head, a stale lookup can
only refuse a request, never serve a deleted repository.

**Identity.** Development credentials (`password`, `password_sha256`) are
accepted only when the server listens on the loopback address. Elsewhere,
users need `password_hash` (PBKDF2-HMAC-SHA256 with a random salt, made by
`worktree hash-password`), or an external identity service (`[identity]`)
that returns the user and their access levels. Verified credentials are
reused for a short time, keyed by a digest of the credentials.

**Ref permissions.** Each repository can have `ref_rules`. A rule names a
ref or a prefix ending in `*` and sets the access levels for creation and
fast-forward updates (`update`), non-fast-forward updates and tag moves
(`force`), and deletion (`delete`). The most specific rule applies. Without
a rule, the defaults are write, force, and write. The push procedure reads
the rules again before each attempt, and the log entry records the rule
version.
