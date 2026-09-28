# 4. Users in storage and the admin API

Status: accepted. Date: September 28, 2026.

## Context

Users lived only in the configuration file. Adding a user or changing
access meant editing the file on every server; in the Vultr deployment,
`./tf apply` and `./rollout.sh`. A management interface needs users that
change without a restart, and an API that it can call without storage keys.

## Decision

**User store.** With `catalog.kind = "storage"`, users can also be kept in
`<prefix>users/<sha256 of the name>.json`, beside the repository catalog
and under the same rules: conditional creation, then CAS only, and a digest
as the key so a client-supplied name never becomes a storage key. A record
holds the name, a salted password hash (the `password_hash` format of
decision 3, with the default iterations), the access levels, and who
changed it last and when. Deletion marks the record deleted and removes its
access; a reused name replaces the deleted record. Plain passwords are never
stored or logged, and no API answer contains a hash.

**Precedence.** Users in the configuration file come first; the store is
consulted only for names the file does not have. A stored user with the same
name as a file user cannot sign in, and the API refuses to create or change
one. The file therefore always keeps a way in (the bootstrap admin). With an
identity service (`[identity]`), the service decides alone and the store is
not used.

**Propagation.** Lookups are reused for `catalog.cache_secs`. The server
that makes a change applies it to its next request; other servers apply it
within that time. The cache of verified hashes is keyed by the credentials
and the hash, so a new password is checked again at once.

**Admin API.** JSON under `/_rustgit/api/`, enabled with `server.admin_api`
(the default, as for the maintenance endpoints):

| Request | Answer |
|---|---|
| `GET me` | `{name, admin}` for any authenticated user |
| `GET repos` | Active repositories with settings and head generation |
| `GET users` | `{managed, users}`: file users (`source: "config"`) and stored users |
| `POST users` | `{name, password, access}`; 201 |
| `PATCH users/<name>` | `{password?, access?}`; `access` replaces all levels |
| `DELETE users/<name>` | 204; a user cannot delete itself |

Everything except `me` needs admin access to every repository
(`"*" = "admin"`). Names use 1 to 64 letters, digits, `.`, `_`, or `-`;
passwords need at least 12 characters; access keys are `*` or valid
repository names. Hashing runs off the request threads.

**Management interface.** `web/` is a Next.js application that signs in
with rustgit credentials, keeps them in an encrypted, httpOnly session
cookie, and calls the API from the server side only.

## Consequences

- Log entries record the permission-rule version of the configuration
  file. A change to a stored user's access does not increase it, as with
  identity-service users; the change itself is recorded only in the user
  record (`updated_ms`, `updated_by`).
- A deleted or changed user can keep its previous access on other servers
  for up to `catalog.cache_secs`.
- Failed sign-ins are not rate-limited, for Git requests or the API alike.

## Evidence

`admin_api::*`, `users::tests::*`.
