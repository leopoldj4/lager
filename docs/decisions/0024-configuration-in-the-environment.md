# 24. A minimal configuration in the environment

Status: accepted; implemented in `crates/core/src/config.rs`. Date:
October 21, 2026.

## Context

Every option is code to load, check, and document, and a choice that each
deployment has to get right. Settings that could live in two places (a
file and a store) need precedence rules and checks against conflicting
combinations. Deployments run on Dokploy, which configures a service
through its environment; files on the host need SSH and a directory per
deployment.

## Decision

The configuration holds only what a server needs to start: its address,
the storage service, the database, the change bus, the web session secret,
the GitHub OAuth app, the SSH host key, and the public host name. A new
option needs a deployment that cannot work without choosing it.

- **Only in the database:** users, their passwords and access levels,
  personal access tokens, SSH keys, and passkeys.
- **Only in the catalog in storage:** the repository name to UUID map, the
  default branch, public reading, and ref rules, managed with `lager repo`.
- **Fixed in code** (`crates/core/src/limits.rs`): all limits, drain and
  shutdown times, the catalog cache time, snapshot thresholds, database
  pool size, the NATS subject prefix and grace period, and session length.
  A push to a missing name creates the repository with `write` access to
  the name.
- **Not in the server:** TLS (a load balancer terminates HTTPS) and request
  forwarding between servers. The web interface is found in
  `share/lager/web` beside the programs. The server that accepted a push
  creates the next automatic snapshot; two that both do keep one through
  CAS.
- The server and `lager` read only `LAGER_*` environment variables, once at
  start. The README lists them. An empty variable counts as unset, so a
  compose file can pass optional ones through as `${NAME:-}`.
- `LAGER_DOMAIN` names the public host once. The passkey origin is
  `https://<domain>` and the clone address `ssh://git@<domain>`; neither has
  its own variable. `localhost[:port]` gives `http://` for development; the
  local clone address is then the SSH listen address.
- A section turns on with its main variable: `LAGER_SESSION_SECRET` for the
  web interface, `LAGER_NATS_URL` for the bus, `LAGER_GITHUB_CLIENT_ID` and
  `_SECRET` for mirrors, `LAGER_SSH_HOST_KEY` for SSH. Listen addresses have
  defaults (`0.0.0.0:8080`, `0.0.0.0:2222`).
- `LAGER_SSH_HOST_KEY` holds the key itself, several lines in double
  quotes.
- On Dokploy, the service's environment holds everything, secrets included;
  the compose file builds `LAGER_DATABASE_URL` from `POSTGRES_PASSWORD`.

## Consequences

- Secrets are in Dokploy's database and backups and visible in its
  dashboard. Dokploy controls Docker on the host, so files there would not
  be out of its reach either.
- `lager` needs the same variables as the server: in a container,
  `docker compose exec` passes them; locally, `.local/lager.env` holds them.
- Only one deployment per server can have SSH on port 22; another one sets
  `LAGER_SSH_PORT`, which the compose file publishes and the clone address
  shows.
- Every server needs PostgreSQL for signed-in requests and storage for
  everything; the first user is created with `lager user create`.
- Without forwarding, a repository's cache can be built on every server
  that the load balancer sends its requests to.
