# 24. Configuration in the environment

Status: accepted; implemented in `crates/core/src/config.rs`. Date:
October 21, 2026.

## Context

The configuration was a TOML file (decision 15). On Dokploy, the file, the
SSH host key, and the PostgreSQL password lived in `/etc/lager` on the
host, outside Dokploy, and were bind-mounted into the containers. Several
deployments on one host would need several such directories, and every
change needed SSH to the host. The public host name was set three times:
for Traefik, as `web.public_origin`, and in `ssh.public_url`.

## Decision

- The server and `lager` read only `LAGER_*` environment variables; the
  file, `-c`, and `LAGER_CONFIG` are removed. The README lists the
  variables. An empty variable counts as unset, so a compose file can pass
  optional ones through as `${NAME:-}`.
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
  dashboard. Dokploy controls Docker on the host, so files there were not
  out of its reach either.
- `lager` needs the same variables as the server: in a container,
  `docker compose exec` passes them; locally, `.local/lager.env` holds them.
- An SSH port other than 22 cannot be shown in the clone address; a
  deployment that publishes another port shows a wrong address.
- Existing `lager.toml` files are ignored; their values move to variables.
