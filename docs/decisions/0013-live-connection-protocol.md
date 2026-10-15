# 13. Live updates for the web interface over WebSockets

Status: accepted; implemented in `crates/server/src/web/live.rs` and
`web/src/lib/live.ts`. Sign-in and read queries: decision 14; pull
requests and the `pull` message: decision 19. Date:
October 10, 2026; revised October 12, 2026.

## Context

The web interface is an interactive single-page application. It shows a
push on every open page as it happens. Pages change the set of watched
repositories as the user navigates. Change events reach every server
through NATS (decision 12).

## Decision

**Transport.** One WebSocket per tab at `GET /-/api/live`, to any server.
It carries the read queries of the pages too, so a page needs one
connection and one sign-in check. Server-sent events would need a new
connection or a second channel for each change of the watched set;
WebTransport needs HTTP/3 over UDP, which hyper/axum do not serve and many
networks block, so it would need a fallback anyway.

**Sign-in.** The session cookie of decision 14, because browsers cannot
set headers on a WebSocket and tokens in URLs reach logs. The server checks
`Origin` on the upgrade (decision 14), so other sites cannot open a socket
with the user's cookie. Without a valid cookie, the socket sees
repositories with `anonymous_read` only.

**Messages.** JSON text messages with a `type`, at most 64 KiB. Client
messages can carry an `id` that the answer repeats.

| Direction | Message | Meaning |
|---|---|---|
| client | `hello {v}` | Protocol version (1). Other versions close the socket. |
| client | `sub {id?, repo}` | Watch a repository. |
| client | `unsub {id?, repo}` | Stop watching. |
| client | `req {id, op, ...}` | A read query (decision 14). |
| server | `welcome {v, server, user}` | Answer to `hello`; `user` is null without a session. |
| server | `subbed {id, repo, generation}` | The current generation. Above the page's: fetch again. |
| server | `unsubbed {id, repo}` | Answer to `unsub`. |
| server | `changed {repo, generation}` | A push. |
| server | `res {id, data}` | Answer to `req`; `data.generation` is the generation it shows. |
| server | `error {id?, repo?, code, message}` | `not_found`, `too_many_subs`, `bad_request`, `busy`, `unavailable`, `internal`, and query-specific codes. |
| server | `resync {repo?}` | Events can be missing; fetch again and send `sub` again (for all repositories without `repo`). |
| server | `heartbeat` | Every 25 seconds. |

**Access.** Events are restricted by repository only: `sub` needs read
access, and every event of the repository goes to every such subscriber.
Ref rules (decision 3) restrict only writes, so events show nothing that
`git fetch` would not. Unknown repositories and missing access both answer
`not_found`. At every heartbeat the server looks the session user up again
and checks each subscription with the current access and repository
settings; a lost one gets `error not_found`, an ended session closes the
socket with code 4401.

**Order on `sub`.** Each connection receives the events of the bus from its
start. On `sub`, the server reads the head generation from storage and then
answers `subbed`; events at or below that generation are dropped. Reading
the head before receiving events would lose a push in between.

**Client.** It keeps the highest generation per repository, from `subbed`,
`changed`, and every `res`, and ignores events at or below it. A newer
generation, `resync`, or a reconnect refetches the repository's queries. A
lost event therefore delays a page, but never leaves it wrong. Queries in
flight during a reconnect are sent again; they only read.

**Server.** One task per connection with a queue of 256 messages and its
own receiver of `Bus::subscribe`. Events never wait for the socket: when
the queue is full, the connection owes a `resync`, and one that still
cannot take it at the next heartbeat is closed with 1013. A connection
that falls 1024 events behind the bus gets `resync`. Limits: 100
subscriptions and 8 queries in flight per connection.

**Lifecycle.** The server sends a WebSocket ping and a `heartbeat` every 25
seconds, and closes connections that sent nothing for 70 seconds.
Browsers do not show pings to scripts, so the client reconnects after 60
seconds without a message. On a clean stop the server closes with code
1012; clients reconnect after a random delay up to min(30 s, 0.5 s · 2ⁿ).

## Consequences

- `-` is reserved as a repository name segment (decision 14).
- Several tabs open several sockets. A `SharedWorker` can share one later
  without a protocol change.
- Each connection filters every event of the cluster. A hub that keeps the
  connections per repository can replace the receivers when the event rate
  needs it, without a protocol change.
- No limit on connections per user yet.
- Access changes reach open subscriptions within one heartbeat.
