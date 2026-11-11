# 29. Activity graph on profiles

Status: accepted; implemented in `crates/core/src/activity.rs`,
`crates/server/src/activity.rs`, `crates/server/src/web/profile.rs`,
`crates/server/src/web/emails.rs`, and `Github::contributions` in
`crates/server/src/github.rs`. Date: November 9, 2026.

## Context

Users want one graph of their work over the last year: what they do here
and what they do on GitHub, private repositories of both included. GitHub
has no way to add activity from elsewhere to its own graph, so the graph
lives here and nothing goes back to GitHub. Users had no email
addresses, so a commit could not be tied to a user.

## Decision

**Profiles.** `/<user>` shows the user's activity of the last 53 weeks
and their repositories that the viewer can read. Anyone may read a
profile (`profile` on the live connection). It shows counts per day only:
no repository names, messages, or commits.

**Email addresses.** A connected GitHub account brings the addresses
GitHub verified (`GET /user/emails`, scope `user:email`) and its two
`noreply` addresses (`<id>+<login>@users.noreply.github.com` and
`<login>@users.noreply.github.com`), which commits made on GitHub use. They
are marked `github`, follow GitHub on each connection and hourly refresh,
cannot be deleted here, and go when the user disconnects GitHub. A token
from before this scope gets only the `noreply` ones until the user connects
again.

A user can also add addresses by hand in the settings (`/-/api/emails`),
kept in lower case, at most 20. Nothing checks those: this server sends no
mail. Claiming someone else's address only adds their commits to the
claimer's count, and only commits in repositories the claimer can read, so
it reveals nothing new.

**Activity here.** After each change on a server, it reads the commits
of the repository's default branch with `git rev-list` since the commit it
read last, back to 375 days ago, and keeps each commit's author email and
day in the author's time zone (`lager_activity_commits`), with the tip it
read (`lager_activity_repos`). A force push or a new default branch reads
the branch again from the start. At start, a server reads the repositories
it never read and forgets deleted ones. A user's count on a day is the
commits with one of their addresses in the repositories they can read,
each commit once however many repositories hold it, plus the pull requests
they opened here. As on GitHub, only the default branch counts. Schema
version 9.

**Activity on GitHub.** With a connected GitHub account, a profile asks
GitHub's GraphQL API for the user's contribution calendar
(`viewer.contributionsCollection`) with the stored token, which includes
private contributions, and keeps the counts per day for an hour
(`lager_github_contributions`). If GitHub does not answer within ten
seconds, the stored counts stay. Once GitHub's counts exist, mirrors no
longer count here: GitHub counts their commits already, both those made
there and those that two-way mirrors sent (decision 28).

## Consequences

- The graph starts full: the first read covers the last year of every
  repository.
- A lost change event leaves a repository's count behind until its next
  change.
- The same commit pushed to a repository here and to a GitHub
  repository that is not a mirror counts twice.
- GitHub's counts include issues, pull requests, and reviews there.
  Here, commits and opened pull requests count.
- Any visitor can see a user's daily counts, including activity in private
  repositories.
- Servers before this decision do not know schema version 9; upgrade every
  server.
