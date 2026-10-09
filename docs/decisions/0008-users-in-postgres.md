# 8. Users in PostgreSQL behind an identity provider trait

Status: accepted; users are managed with `worktree user` instead of the
admin API since [decision 10](0010-no-admin-api.md). Date: October 9, 2026.

## Context

Decision 4 kept managed users as JSON records in the storage service, one
object per user. That works for sign-in by name, but every other question
(all users, who can access a repository, teams, tokens later) means listing
and reading every record, and each change needs its own CAS loop. Other
servers saw changes only after `catalog.cache_secs`. Identity checks were
also spread over one type with three kinds of user (file, storage,
identity service).

## Decision

**Trait.** `crate::identity::IdentityProvider` checks a user name and
password and answers with the user's access levels
(`Identity::User { name, access }`). `Ok(None)` means the provider does not
know the name. A provider that manages users also lists, creates, changes,
and deletes them; the defaults refuse. `reload` takes the users of a
changed configuration file. The server builds one provider at start:

| Configuration | Provider |
|---|---|
| `[identity]` | The external service decides alone (unchanged) |
| `[database]` | File users first, then PostgreSQL |
| neither | File users only; the admin API cannot change users |

`[identity]` and `[database]` together are refused.

**PostgreSQL.** `[database]` takes a connection URL (TLS through
`sslmode`), `max_connections` (8), and `timeout_secs` (5) for a connection.
Two tables: `worktree_users` (name, salted password hash in the format of
decision 3, created and updated times, `updated_by`) and
`worktree_user_access` (user, repository name or `*`, level). Deleting a user
deletes the row and, through the foreign key, its access. Changes run in a
transaction; an update locks the user row.

**Schema changes.** The SQL lives in the program as an ordered list and
`worktree_schema` records the applied versions. At start the server takes an
advisory lock, applies missing versions, and refuses to start against a
newer schema or an unreachable database. Applied entries are never edited.

**Lookups.** Every sign-in of a database user reads the database; there is
no lookup cache, so changes apply to the next request on every server. The
cache of verified password hashes (keyed by credentials and hash, 60
seconds) stays, and verification now runs off the request threads.

**Precedence and API.** As in decision 4: file users come first, a database
user with a file user's name cannot sign in, is not listed, and cannot be
created or changed. The admin API answers are the same, with
`source: "database"` instead of `"storage"`.

## Consequences

- The user records of decision 4 under `<prefix>users/` are no longer read.
  Users there must be created again; the records can be removed.
- A database outage fails sign-in of database users with 503. File users
  and anonymous reads keep working, and the health check does not fail, so
  a load balancer keeps all servers.
- Access is fixed when the request signs in. A push no longer sees a change
  to a file user's access made during the push, as was already the case for
  identity-service and stored users.
- Local mode starts PostgreSQL next to SeaweedFS.
