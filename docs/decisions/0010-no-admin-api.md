# 10. No admin API and no admin access level

Status: accepted. Date: October 9, 2026.

## Context

The admin API (decisions 4 and 6) let users with `"*" = "admin"` manage
users, repositories, and tokens over HTTP, and the `admin` level also
allowed snapshots and cache rebuilds over HTTP. That made every Git server
a management endpoint, guarded by one more access level and the
`server.admin_api` switch, and needed a command-line client with saved
credentials. With users in PostgreSQL (decision 8), the people who manage
users already have the server configuration.

## Decision

- `/_lager/api/` (`me`, users, repositories, tokens), `POST
  /_lager/snapshot`, `POST /_lager/rebuild`, and `server.admin_api`
  are removed. A configuration that sets `admin_api` no longer loads.
- The `admin` access level is removed. Levels are `none`, `read`, `write`,
  and `force`. A configuration, database row, or identity-service answer
  with `admin` is refused. No server runs the schema of decisions 8 and 9
  yet, so it becomes one initial schema version without `admin`; there is
  nothing to upgrade.
- `lager login`, `logout`, and the client for the API are removed.
- Users and tokens are managed with `lager user` and `lager token`
  instead of the API of decisions 8 and 9. They read the server configuration and change the database directly, as
  `lager repo` changes storage directly. `user create` and
  `user set-password` ask for the password (twice on a terminal) or read it
  from standard input; `updated_by` records `cli:$USER`.

## Consequences

- Managing users, tokens, and repositories needs the server configuration
  (database and storage credentials), not a lager user.
- Ref rules can no longer reserve changes for a group above `force`: the
  highest rule level is `force`, which every user with force access has.
- To rebuild a server's cache, remove its cache directory; the server
  recovers from storage. Snapshots run automatically or with
  `lager snapshot`.
