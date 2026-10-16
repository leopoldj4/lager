# 20. Read-only mirrors of GitHub repositories

Status: accepted; implemented in `crates/core/src/github.rs`,
`crates/server/src/github.rs`, `crates/server/src/web/github.rs`, and
`web/src/routes/[-].settings.github.tsx`. Date: October 15, 2026.

## Context

Users keep code on GitHub and want it here too, without a second remote
to push to. For now the sync goes one way: GitHub is the source, and the
copy here must never drift from it, so nothing here may change it.

## Decision

**Connecting.** `[github]` names a GitHub OAuth app (`client_id`,
`client_secret`); its callback URL is `<origin>/-/api/github/callback`.
**Settings** (`/-/settings/github`) has a **Connect with GitHub** button:
`GET /-/api/github/connect` sends the signed-in user to GitHub with a
random state in a ten-minute cookie (`SameSite=Lax`, path
`/-/api/github`), and the callback checks the state, exchanges the code,
and keeps the token. The scope is `repo`: OAuth apps have no read-only
scope for private repositories. A GitHub App would allow read-only
access, but needs installations and expiring tokens; it can replace the
OAuth app later without a change to mirrors.

**Data.** PostgreSQL, schema version 5: `worktree_github_accounts` (one
GitHub account and its token per user) and `worktree_github_mirrors`
(repository UUID, name, user, `owner/name` on GitHub, when the next sync
is due, the last success, the last error). The token is stored in plain
text: Git needs it to fetch. The catalog record of a mirror names its
GitHub repository (`mirror`), so the push path, which never reads the
database, knows it is read-only.

**Mirroring.** Picking a GitHub repository creates `<user>/<name>` with
GitHub's default branch and `mirror` set, then a mirror row due at once.
A retry after the catalog change finds the record and adds the row. An
existing repository with that name is refused.

**Read-only.** `Directory::access` gives at most `read` on a mirror, so
pushes, merges, and closing pull requests are refused; a push gets
"read-only mirror of github.com/…; push to GitHub". Opening a pull request
is refused too. The web interface shows "Mirror of …" and no pull request
tab. **Stop syncing** removes `mirror` from the record and the row: the
repository stays and takes pushes again.

**Syncing.** Every server with `[github]` looks for due mirrors every ten
seconds, and at once after a new mirror or connection. It takes one with
`UPDATE … FOR UPDATE SKIP LOCKED` that moves its due time 30 minutes
ahead (a lease), up to four at a time. A sync:

1. checks that the name still maps to this repository and GitHub
   repository, else drops the row;
2. reads GitHub's branches, tags, and `HEAD` with `git ls-remote`;
3. compares them with the cache's refs; if any differ, fetches those refs
   into the cache with `git fetch --stdin` and refspecs without a
   destination, so only objects arrive, and checks that every wanted
   object is there;
4. changes the refs with the push procedure as a push by the server
   (user `:github`, operation `push`): creations, updates, force updates,
   and deletions, exactly as on GitHub. Its access check gives `force`
   only while the name still maps to this mirror;
5. sets the default branch to GitHub's if it changed;
6. records success or the error, and makes the mirror due in 60 seconds.

The token reaches Git as an `http.extraHeader` in `GIT_CONFIG_*`
environment variables, never in arguments or URLs; only `https` is
permitted. Fetched objects go through `transfer.fsckObjects` and the
object limits of the push procedure.

## Consequences

- Changes reach the mirror within about a minute, by polling; GitHub
  webhooks would need a public address and are not used.
- Each mirror costs one `ls-remote` per minute per deployment, and a fetch
  only when something changed.
- Only branches and tags are mirrored; GitHub's pull requests, issues, and
  `refs/pull/` are not.
- A server that stops during a sync leaves the mirror leased for up to 30
  minutes. Two servers that sync the same mirror are ordered by the head
  CAS; the loser records an error and the next sync passes.
- A first sync of a large repository has the push deadline of 120 seconds
  for building and uploading its pack.
- Disconnecting GitHub keeps the mirrors; they show an error until the
  user connects again.
- Servers before this decision ignore `mirror` and accept pushes to
  mirrors: upgrade every server before the first mirror.
