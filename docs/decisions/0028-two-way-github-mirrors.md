# 28. Two-way GitHub mirrors

Status: accepted; implemented in `crates/server/src/github/push.rs`,
`crates/server/src/github/changes.rs`, `crates/server/src/repository/push.rs`,
and `crates/server/src/web/pulls.rs`. Date: November 9, 2026.

## Context

A mirror (decision 20) was read-only: a push was refused, and a pull request
copied from GitHub (decision 26) could only be changed on GitHub. Users want
to work here and have it on GitHub. Two writers without a common
compare-and-swap diverge: if a push were accepted here and sent to GitHub
later, GitHub could refuse it (a protected branch, a push in between), and
an accepted push would have to be taken back.

## Decision

**GitHub stays the source.** A change here goes to GitHub first and happens
here only once GitHub accepted it. Only the sync brings GitHub's state here,
as before, so whatever fails after GitHub accepted is repaired like any
change made on GitHub. While GitHub is unavailable, mirrors take no changes.

**The user's own account.** Every change goes to GitHub with the GitHub
account that the acting user connected, not the mirror owner's. GitHub
checks its own permissions and rules and names the right person. A user
without a connected account is told to connect one. The OAuth scope is now
`repo workflow`: GitHub takes pushes that change workflow files only with
`workflow`. Accounts connected before this decision push everything else and
are told to connect again for those.

**Access.** `Directory::access` no longer limits a mirror to `read`. A push
needs `write` here (and the ref rules, and `force` for a forced update) and
GitHub's permission there. `PushAccess.mirror` tells the push procedure that
the repository is a mirror: a change without a forward is refused, so
nothing but the sync changes a mirror on its own.

**Pushes.** Authorizing a push to a mirror reads the user's token and gives
the push a `Forward`. The push procedure calls it after its own checks and
the pack upload, before the log entry and the head CAS, once per push:

1. `git push --atomic --porcelain` from the cache to GitHub, with
   `--force-with-lease=<ref>:<value here>` for every ref. GitHub takes the
   change only from the value that the client's change was checked against,
   so a force push or deletion never removes commits the mirror has not
   seen, and a creation fails if the ref exists there.
2. A refused ref gets GitHub's reason: `remote: error:` lines such as a
   protected branch's rule, or "GitHub has changes that are not here yet;
   fetch and push again" for a failed lease, which also pokes the mirror
   (decision 27) so the fetch finds the commits.
3. After any failure, also a lost answer or the push deadline, `git
   ls-remote` reads the refs: if GitHub has the new values, the push was
   accepted. This also makes a repeated push succeed.

Then the CAS happens as for any push, and the entry names the user. After
it the mirror is poked, and again four seconds later, so the sync soon
brings what GitHub changed with the push, such as the head of a pull
request, which GitHub moves a moment after the branch. Later
attempts after a CAS conflict do not push to GitHub again. If the sync
brought the same change first, the attempt finds the refs at their new
values and answers success. If the push still fails here after GitHub
accepted it (storage unavailable, the deadline, an unknown CAS result), the
client is told "pushed to github.com/…, but not applied here yet; it arrives
with the next sync". Success is not claimed: a read
after a successful push must include it.

**Sync.** A sync now reads the refs here before GitHub's. A push here
reaches GitHub first, so GitHub's answer is never older than the refs it is
compared with, and a sync cannot take back a push that ran beside it. A sync
whose ref change is refused because a push made the same change counts as
done.

**Pull requests.** The pull requests of a mirror are GitHub's. With
`[github]`, the web changes of a mirror go to GitHub's REST API:

| Here | GitHub |
|---|---|
| open | `POST /repos/{owner}/{name}/pulls` |
| comment | `POST …/issues/{n}/comments` |
| close, reopen | `PATCH …/pulls/{n}` |
| merge | `PUT …/pulls/{n}/merge` with `sha` and `merge_method` |

GitHub's answer is written with the import of decision 26, so the page shows
it at once and the next sync finds it unchanged; a comment keeps GitHub's ID
and is not copied twice. `sha` is the head the page showed, so GitHub refuses
a merge of commits nobody reviewed (`stale`), as here. GitHub writes the
merge commit; the sync brings it, and no receipt ref is needed. A merge
needs `write` here; closing is shown to the author's GitHub account and to
users with `write`; GitHub decides. A comment, close, or merge needs the
copy here, because GitHub numbers issues with pull requests. The page shows
whether an open pull request merges without conflicts, computed here from
GitHub's head.

After **Stop syncing**, copied pull requests stay as they are and cannot be
changed here, as before.

## Consequences

- A push to a mirror takes one push to GitHub longer, within the push
  deadline of 120 seconds, and holds the repository's write lock on its
  server meanwhile.
- A push whose lease fails because the mirror lags GitHub by a few seconds
  must be repeated after a fetch.
- The entry of a push that the sync brought first names `:github`, not the
  user.
- GitHub's required reviews and checks are not shown here; a merge they
  block is refused with GitHub's message.
- GitHub has no request IDs: a comment whose answer is lost and that is sent
  again appears twice.
- Who closed or reopened a pull request here is still not shown (decision
  26).
- Collaborators with `write` here and no access on GitHub cannot push;
  GitHub refuses them.
- Servers before this decision refuse pushes to mirrors. They are safe
  beside newer ones, but upgrade every server for the same answers.
