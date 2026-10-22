# 26. Pull requests of GitHub mirrors, and Markdown messages

Status: accepted; implemented in `crates/server/src/github/pulls.rs`,
`crates/server/src/github.rs`, `crates/core/src/pulls.rs`, and
`web/src/components/repo/markdown.tsx`. Date: October 22, 2026.

## Context

A mirror (decision 20) followed GitHub's branches and tags, but not its
pull requests, which is where most of the conversation about the code
happens. The pull request pages (decision 19) can show them if the data
lands in the same tables. GitHub's descriptions and comments are Markdown,
and ours were plain text.

## Decision

**One way, like the branches.** GitHub is the source. A pull request copied
from GitHub cannot be commented on, closed, reopened, or merged here, also
after **Stop syncing**; its page links to GitHub for that. Reviews, comments
on lines, labels, and assignees are not copied.

**Refs.** The mirror sync reads `refs/pull/*/head` with the branches and tags
and moves them with the same push by the server, deletions included. GitHub
keeps these refs for every pull request, from forks too, so their commits
arrive without a fetch from the fork. At most 1000 of them change per sync,
the highest numbers first; a first sync of a larger repository continues in
the next ones.

**Data.** Schema version 7. `lager_pulls.github_url` (the page on GitHub)
marks a copied pull request: its `author` and `merged_by` and the authors of
its events are GitHub logins, and the page links them to GitHub. Its number
is GitHub's, and `lager_pull_numbers` is raised to the highest one, so local
pull requests after **Stop syncing** come after them. The unique index of
open pull requests per branches leaves copied ones out: GitHub keeps its own
rule, and a sync sees changes in some order. Forks' branches are
`owner:branch` (`unknown:branch` for a deleted fork). `merge_base` is the
merge base of GitHub's base and head commits if the cache has both, else
GitHub's base commit. `lager_pull_events` gets `github_id` (GitHub's comment
ID, unique per repository) and `edited_at`. The timeline is ordered by
`created_at`, which copies take from GitHub. `PullService` leaves copied
pull requests alone: their refs and states follow GitHub, not the branches
here.

**Import.** After the refs, each sync reads GitHub's REST API with the
mirror user's token, at most ten pages of 100:

1. pull requests by `updated` descending, until one at or before the
   newest `updated_at` seen (`pulls_since`); they are written oldest first;
2. while the first import runs (`pulls_import_page`), the next pages of all
   pull requests by `created` descending; new pull requests move the pages,
   which only repeats some;
3. once that is done, comments of the repository by `updated` ascending
   since the newest one seen (`comments_since`, and `comments_page` for more
   than a page with one time). Comments on issues are skipped; a comment on a
   pull request not yet here stops the pass until the next sync.

A pull request is written with one `INSERT … ON CONFLICT DO UPDATE … WHERE
… IS DISTINCT FROM …`, so an unchanged one changes nothing. A change raises
its version and is published like a local one (`lager.repo.<id>.pull`).
Differences from the last copy become events: a new head is a push, and
closing, reopening, and merging are events at GitHub's times without an
author, except the merger, which one more request reads for merges seen after
the first import. A first copy shows only how a pull request ended. The
first page of steps 1 and 3 sends the last ETag; GitHub answers an unchanged
list with 304, which does not count against the rate limit. A used-up rate
limit is the mirror's error until it resets.

**Markdown.** Descriptions and comments of all pull requests are Markdown
with GitHub's extensions. Unlike files (decision 16), they keep HTML, which
GitHub's pull request templates and image uploads use, cleaned with GitHub's
rules (`rehype-sanitize`): no scripts, styles, event handlers, forms, or
`javascript:` links. `#123` links the pull request, a full commit ID the
commit, and `@login` the GitHub account on copied pull requests. Relative
links start at the root of the target branch. The forms have a preview.
Texts can have up to 65,536 characters, GitHub's limit.

## Consequences

- Each mirror costs two more requests per minute, usually answered with 304,
  and one per new merge; a first import of 5,000 pull requests takes five
  minutes, then the comments follow at 1,000 a minute.
- Comments deleted on GitHub stay here, and who closed or reopened a pull
  request is not shown.
- Images uploaded to GitHub load from GitHub; those of private repositories
  need a GitHub session in the browser.
- A token without access to a repository's pull requests fails the sync
  after the refs have moved; the error shows on the mirror.
- Servers before this decision do not know schema version 7 and refuse to
  start; upgrade every server.
