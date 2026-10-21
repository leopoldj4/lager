# 18. Repositories under users

Status: accepted; implemented in `validate_repo_name` in
`crates/core/src/repo.rs` and `Identity::access` in
`crates/core/src/identity/mod.rs`. Date: October 15, 2026.

## Context

Repository names were any `/`-separated segments, so `app`, `alice`, and
`alice/app` were all valid, and a user could only create repositories
an administrator had granted by name or with `*`. Users should be able to
create their own repositories without either.

## Decision

A repository name is a user name followed by at least one more segment:
`alice/app` and `alice/tools/app` are valid, `app` and `alice` are not.
The first segment follows the rules for user names (1 to 64 letters,
digits, `.`, `_`, or `-`, not starting with `.` or `-`); the others keep
the earlier segment rules. The check applies everywhere a name is
checked: Git and raw-file URLs, web queries, the catalog, and access
entries (`*` stays valid).

**Owner access.** A user has `force` on every repository under their
name, whatever their grants say, so a push to a missing `<user>/...` name
creates it (decision 5). Grants of decision 8 add access to other names.
A token with its own levels (decision 9) still limits the owner: the
level is the lower of the user's and the token's. The user does not have
to exist for a name to be valid; until they do, only grants reach it.

## Consequences

- Repositories with one-segment names stay in the catalog and in
  `lager repo list`, but no URL reaches them and their names cannot be
  granted. Move one with `lager backup export` and
  `lager backup import --name <user>/<name>`, then delete the old one.
- The local development repository is now `dev/demo`.
- A user who is deleted and created again, or a new user with an old
  name, owns the repositories under that name.
- Grants for a user's own repositories cannot lower their access.
