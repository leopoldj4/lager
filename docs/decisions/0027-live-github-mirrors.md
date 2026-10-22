# 27. Live GitHub mirrors

Status: accepted; implemented in `crates/server/src/github.rs`,
`crates/server/src/github/hooks.rs`, `crates/server/src/web/github.rs`, and
`crates/server/src/web/live.rs`. Date: October 22, 2026.

## Context

A mirror synced once a minute (decision 20), so a change on GitHub, such as
a new comment on a pull request (decision 26), took one to two minutes to
show. Pages update as soon as a sync writes a change; the sync was what
was slow. GitHub can call us on every change, but only for repositories
whose admin adds a webhook, and only if the site has a public address.
Many mirrors are of repositories their user does not administer.

## Decision

Both, sharing one mechanism: a **poke** asks for a sync now. It sets
`poked_at` and makes the mirror due at once, unless a sync holds its lease
(a due time more than a sync interval ahead); a sync that started before
the poke (`claimed_at`, set by the claim) makes the mirror due again when it
finishes. Servers look for due mirrors every two seconds, and at once on a
poke on the same server. Schema version 8.

**Watching.** A live subscription (decision 13) to a mirror holds a watch on
its server. The first watch of a mirror on a server pokes it and sets
`watched_until` 90 seconds ahead; the server renews it every 30 seconds while
any page there shows the mirror. A watched mirror is due five seconds after
its last sync instead of 60. Unchanged answers cost GitHub's rate limit
nothing (304 to an ETag); each sync still runs one `git ls-remote`.

**Webhooks.** If the site has a public `https` origin (`LAGER_DOMAIN`), a sync
of a mirror without a webhook adds one: `POST /repos/{owner}/{name}/hooks`
for `push`, `create`, `delete`, `pull_request`, and `issue_comment`, with
`<origin>/-/api/github/hook/<repository UUID>` and a random secret, both
kept in the mirror's row (`hook_id`, `hook_secret`). The `repo` scope covers
this; GitHub permits it to the repository's admins only. A refusal is
recorded (`hook_checked_at`) and tried again a day later; a webhook left
from an earlier attempt for the same address is replaced, since its secret
is unknown. **Stop syncing**, deleting the repository, and a sync that finds
the mirror gone remove the webhook from GitHub while the token permits.

`POST /-/api/github/hook/<uuid>` checks `X-Hub-Signature-256` (HMAC-SHA256
of the body with the mirror's secret, compared in constant time) and pokes
the mirror: `202`, `200` for GitHub's `ping`, `401` for a wrong signature,
`404` for a mirror without a webhook. Bodies may have GitHub's 25 MB. The
delivery's content is not used: the sync reads GitHub as always, so a
repeated, reordered, or lost delivery changes nothing but the timing.

## Consequences

- A page shows a change on GitHub within about five to ten seconds, and
  within a few seconds with a webhook, which also covers mirrors no page
  shows.
- A watched mirror costs one `ls-remote` and two requests answered with 304
  every five seconds; the watch ends 90 seconds after the last page closes.
- GitHub's lists can lag its changes by a few seconds; the next sync of a
  watched mirror catches up.
- Local runs have no public origin and add no webhooks.
- Servers before this decision do not know schema version 8; upgrade every
  server.
