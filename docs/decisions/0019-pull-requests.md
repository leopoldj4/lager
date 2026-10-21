# 19. Pull requests

Status: accepted; implemented in `crates/core/src/pulls.rs`,
`crates/server/src/pulls.rs`, `crates/server/src/web/pulls.rs`, and
`web/src/components/repo/`. Date: October 13, 2026.

## Context

The design kept pull requests as a later extension. A pull request has two
kinds of data. Its commits, its ref, and its merge are Git state, which
changes only through the log and the head CAS. Its title, description,
state, and comments are not Git state: they change often, need queries
such as "the open pull requests of a repository", and must not grow the
head (decision 7). A push must not depend on anything but storage.

## Decision

**Where the data lives.** PostgreSQL holds the pull requests, keyed by
repository UUID, so a reused name starts without the old ones:
`lager_pulls` (number, title, body, author, source and target branch,
state, the source and target commits last recorded, their merge base, the
merge), `lager_pull_events` (comments, pushes to the source branch,
closing, reopening, merging), and `lager_pull_numbers` (the last number
per repository). It is schema version 4. Every change raises the pull
request's `version`. One open pull request per source and target branch.

Storage holds `refs/pull/<n>/head`, which the server keeps at the source
branch of each open pull request, so its commits stay reachable when the
branch moves or is deleted, and survive snapshots. `RefName` accepts
these refs; a client push to `refs/pull/` or `refs/server/` is refused.
Clients can fetch `refs/pull/` (`git fetch origin pull/7/head`).

**Two new operations.** `pull-refs` moves refs of pull requests; its user is
`:server`, which no user name can be. `merge {pull}` is a merge by the
server: the target branch and, if it lagged, the pull request's ref. Both
run the push procedure (§6 of ARCHITECTURE.md) as a push by the server
(`Origin::Server`): the same object checks, packs, CAS, retries, and
unknown-result handling. Refs of pull requests need no access level; a
merge needs the merging user's level for the target branch under its ref
rules. Servers older than this decision cannot read these entries or
refs: upgrade every server before the first pull request.

**Following pushes.** A push never waits for the database. After this
server accepts a push, `PullService::run` takes the change event, records
new source and target commits of the repository's open pull requests
(conditional on the recorded ones, so a second server records nothing),
adds a push to the timeline, and moves their refs with one `pull-refs`
entry. A pull request whose source a push brought into the target is
marked merged by a push (`merged_by` null). Other servers do not repeat
this for remote events. Lost events only delay it: the page of a pull
request shows the branches themselves and asks for the same work.

**Merging.** `pull_merge` names the source commit the page showed; if the
branch moved, the merge is refused (`stale`), so nothing unreviewed is
merged. The server merges with `git merge-tree --write-tree` in the cache,
writes a merge commit (two parents) or a squash commit (one parent, the
pull request's author as author), and pushes it. If another push moved
the target branch, it merges again against it, up to four times. The
client's request UUID makes a retry answer the first merge. The database
is updated after the CAS. The same CAS creates
`refs/server/pull/<n>/receipt`, a receipt commit whose JSON message
records the reviewed head, merge commit, merge base, actor, and request
UUID. Its parents retain the merge and source commits. Background sync
and merge retries use this receipt to finish the database update,
including squash merges and concurrent closing. The ref survives
snapshots and branch deletion; creating it only once prevents two servers
from merging the same pull request twice. A source-ref read guard is
checked against every CAS attempt, so a concurrent source push requires
another review. The receipt commit is outside the target branch history.

The receipt is not `refs/pull/<n>/merge`: on GitHub that name is a test
merge of an open pull request, which CI fetches, and a receipt under it
would mislead such tools. Clients never need the receipt, so refs under
`refs/server/` are hidden from them (`transfer.hideRefs`): not
advertised, fetched, or pushed. Users have no email address, so commits
by the server have an empty one.

**Web interface.** The live connection (decision 13) carries the reads
`pulls`, `pull`, and `compare` and the changes `pull_create`,
`pull_comment`, `pull_close`, `pull_reopen`, and `pull_merge`. Changes
need a session; creating and commenting need read access, closing needs
authorship or write access, merging needs write access. The server sends
`pull {repo, number, version}` to subscribers of the repository, through
NATS subject `lager.repo.<id>.pull`. `pull` answers the comparison that
shows the changes (`base`, `head`) and, for open pull requests, whether
they merge cleanly; the check writes its objects to a temporary directory.

Pages: `/<repo>/-/pulls`, `/-/pulls/closed`, `/-/pull/<n>` with
`/commits` and `/files`, and `/-/compare/<base>...<head>`, which opens one.

## Consequences

- Same-repository pull requests only. Opening one with
  `git push origin topic:refs/for/main` fits the `proc-receive` hook
  (decision 1) but is not done. Pull requests from forks would copy the
  fork's objects with a push by the server into the target repository.
- Comments belong to the conversation, not to lines. Earlier heads of a
  pull request stay in its timeline, but only the current head has a ref:
  once data removal deletes unreachable packs, old heads can be lost.
  Comments on lines need a ref per reviewed head first.
- Each push to a branch of an open pull request adds one log entry.
- A sync reads the open pull requests of the repository after each of the
  server's pushes, and the repository tabs read their counts.
- Titles, descriptions, and comments are plain text.
