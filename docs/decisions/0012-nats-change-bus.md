# 12. Change events over NATS

Status: accepted. Date: October 10, 2026.
`subject_prefix` and `health_grace_secs` are fixed by [decision 15](0015-minimal-configuration.md).

## Context

After an accepted push, a server told the other servers with
`POST /_lager/notify` (`{repo, generation}`) to each address in
`routing.servers`, authenticated with `routing.peer_token`. Requests were
not retried and their results were ignored. Each push cost one request per
other server, a stopped server missed events without notice, and only
servers in the fixed list received them.

A web interface will show pushes as they happen (decision 13). Each browser
connects to any server, so each server must receive the events of all
servers.

## Decision

- **NATS core publish/subscribe**, with no JetStream and no stored
  messages. `[bus]` takes one `url`, a `subject_prefix` (`lager`), and
  `health_grace_secs` (5). Without `[bus]`, events stay on the server that
  accepted the push.
- **Event.** After the head CAS, the receipt, and a change of the default
  branch, the server publishes `{repo, generation, server}` as JSON to
  `<prefix>.repo.<id>.changed`. Publishing never delays the push: it runs
  in its own task with a 2 second limit, and failures only count
  `bus_publish_failures`.
- **Receipt.** Each server subscribes to `<prefix>.repo.*.changed`. The
  connection uses NATS `no_echo`, so a server never receives its own
  events; `server` is only for diagnostic records, because server IDs
  (default `local`) need not be unique. It passes events of all servers,
  including its own, to local subscribers through a channel in the
  process (`crate::bus::Bus::subscribe`). The cache update, formerly
  `notify_handler`, is one subscriber: for events of other servers it
  brings the repository's cache to the storage head in the background.
- **Events are hints.** A lost, repeated, or late event never changes a
  read result, because every request reads the storage head. Receivers
  compare generations, not arrival order: two servers can publish for the
  same repository, and NATS orders messages only per publisher.
- **Health.** `/_lager/health` answers 503 `bus unavailable` before the
  first connection and when the connection is down for longer than
  `health_grace_secs`. The server starts while NATS is unavailable and
  connects in the background; the client reconnects without limit and
  keeps its subscriptions.
- **Removed.** `POST /_lager/notify`, `routing.notify`, and
  `routing.peer_token`. `routing.servers` and `routing.forward` stay for
  request forwarding and the preferred server.
- `/_lager/metrics` stays without sign-in. New counters: `bus_published`,
  `bus_publish_failures`, `bus_received`, `bus_invalid_events`, and
  `health_bus_failures`.

## Consequences

- NATS is one address and one server. When it is down for longer than the
  grace period, every server fails its health check at the same time, and
  Git service stops with it, although storage and Git still work. A DNS
  name in front of several NATS servers gives failover without a
  configuration change.
- Configuration files with `routing.notify` or `routing.peer_token` are
  refused (unknown fields); remove the two keys.
- Events published while a server is disconnected are lost for that
  server. Its caches catch up on the next request; web clients resync
  (decision 13).
- Every server receives every event. Subscriptions per repository are
  possible later if the event rate needs them.
- Local mode starts `nats-server` on 127.0.0.1:4222. An existing
  `.local/lager.toml` needs a `[bus]` table added by hand.
