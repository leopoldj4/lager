# 15. A minimal configuration file

Status: accepted. Date: October 13, 2026.

## Context

The configuration file had grown to about sixty options. Several things
could live in the file or in a store: users in `[[users]]` or in
PostgreSQL, repositories in `[[repos]]` or in the catalog in storage
(`catalog.kind`), and identities from an external service
(`[identity]`). Each pair needed precedence rules, reload code, and
checks that forbade some combinations. Limits, timings, TLS, a fixed
server list for request forwarding (`[routing]`), and customer limits
(`[[customers]]`) were options too, with defaults that hardly anyone
changed.

## Decision

- **Only in the database:** users, their passwords and access levels, and
  personal access tokens. `[[users]]`, development passwords,
  `lager hash-password`, and `[identity]` are removed.
- **Only in the catalog in storage:** the repository name to UUID map, the
  default branch, `anonymous_read`, and ref rules. `[[repos]]` and
  `catalog.kind` are removed. `lager repo set-ref-rule`,
  `delete-ref-rule`, and `show` manage ref rules; `lager backup import
  --name` gives an imported repository a name in the catalog.
- **Only in the file:** `server.listen`, `server.cache_dir` (default
  `/var/cache/lager`), the storage service (`[storage]`: endpoint,
  region, bucket, keys, prefix), `database.url`, `bus.url`, and
  `web.session_secret`.
- **Fixed in code** (`crates/core/src/limits.rs`): all former `[limits]`,
  drain and shutdown times, the catalog cache time, snapshot thresholds,
  database pool size, NATS subject prefix and grace period, and session
  length. A push to a missing name creates the repository with `write`
  access to the name.
- **Removed:** customers and their limits, TLS in the server (a load
  balancer terminates HTTPS; plain HTTP is permitted on every address),
  `[routing]` and request forwarding, `web.origins` (the Vite development
  server forwards with its own host, which passes the origin check), and
  `web.dir`: the interface is found in `share/lager/web` beside the
  programs, where the container image installs it.
- **Snapshots** without a server list: the server that accepted a push
  creates the next automatic snapshot. Two servers that both do keep one
  snapshot through CAS.
- The configuration file is no longer reloaded; nothing in it changes at
  run time. Log entries record permission rule version 0.

## Consequences

- Every server needs PostgreSQL for signed-in requests and storage for
  everything, as before; there is no way in without the database.
- The first user is created with `lager user create`. The local script
  does this for `dev`.
- Existing configuration files do not load until rewritten. Repositories
  in `[[repos]]` must be moved to the catalog with `lager backup export`
  and `lager backup import --name`.
- Catalog records of earlier versions keep loading: their `customer` field
  is ignored.
- Without forwarding, a repository's cache can be built on every server
  that the load balancer sends its requests to.
