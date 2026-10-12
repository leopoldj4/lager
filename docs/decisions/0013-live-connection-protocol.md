# 13. Live updates for the web interface over WebSockets

Status: accepted; not implemented. Browser sign-in (sessions) is still
open. Date: October 10, 2026.

## Context

The web interface is an interactive single-page application. It shows a
push on every open page as it happens. Pages change the set of watched
repositories as the user navigates. Change events reach every server
through NATS (decision 12).

## Decision

**Transport.** One WebSocket per tab at `GET /api/live`, to any server.
Server-sent events would need a new connection or a second channel for each
change of the watched set; WebTransport needs HTTP/3 over UDP, which
hyper/axum do not serve and many networks block, so it would need a
fallback anyway.

**Sign-in.** A session cookie (HttpOnly, `SameSite=Lax`), because browsers
cannot set headers on a WebSocket and tokens in URLs reach logs. The
server checks `Origin` against configured origins on the upgrade, so other
sites cannot open a socket with the user's cookie. Without a cookie, a
socket can watch repositories with `anonymous_read` only.

**Messages.** JSON with a `type`. Client messages can carry an `id` that
the answer repeats.

| Direction | Message | Meaning |
|---|---|---|
| client | `hello {v}` | Protocol version. |
| client | `sub {id, repo, since}` | Watch a repository; `since` is the last known generation or null. |
| client | `unsub {id, repo}` | Stop watching. |
| server | `welcome {v, server}` | Answer to `hello`. |
| server | `subbed {id, repo, generation}` | The current generation. Above `since`: the client fetches again. |
| server | `changed {repo, generation, refs?}` | A push. `refs` is optional and left out for large pushes. |
| server | `error {id, repo, code}` | `forbidden`, `not_found`, or `too_many_subs`. |
| server | `resync {repo?}` | Events can be missing; send `sub` again (for all repositories without `repo`). |

**Access.** Events are restricted by repository only: `sub` needs read
access to the repository, and every event of the repository goes to every
such subscriber. Ref rules (decision 3) restrict only writes, so `refs`
shows nothing that `git fetch` would not. When access changes, the server
checks the affected subscriptions again and sends `error forbidden`.

**Order on `sub`.** The server first receives events for the repository,
then reads the head generation from storage, then answers `subbed`. Events
at or below that generation are dropped. The other order loses a push that
lands between the read and the subscription.

**Client.** It keeps the last generation per repository and ignores events
at or below it. A gap, `resync`, or reconnect makes it send `sub` with
`since` again. A lost event therefore delays a page, but never leaves it
wrong.

**Server.** One task per connection with a bounded queue. A hub per server
keeps the connections per repository and reads `Bus::subscribe`. A full
queue never blocks the hub: the connection gets `resync`, and a connection
that stays behind is closed. Limits on subscriptions per connection and
connections per user.

**Lifecycle.** The server sends a WebSocket ping and an application
heartbeat about every 25 seconds; browsers do not show pings to scripts,
so the client reconnects after two missed heartbeats. On a clean stop the
server closes with code 1012; clients reconnect after a random delay up to
min(30 s, 0.5 s · 2ⁿ).

## Consequences

- Needs browser sign-in with sessions and a read API for refs, commits,
  and trees: `changed` only says what to fetch again.
- Needs the `ws` feature of axum.
- `/api/` must be reserved so that no repository name hides it.
- Several tabs open several sockets. A `SharedWorker` can share one later
  without a protocol change.
