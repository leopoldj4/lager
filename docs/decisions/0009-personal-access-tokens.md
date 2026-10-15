# 9. Personal access tokens

Status: accepted; tokens are managed with `worktree token`, and `login` is
gone, since [decision 10](0010-no-admin-api.md). Date: October 9, 2026.

## Context

Git clients send HTTP Basic credentials and credential helpers keep one
fixed secret, so the account password ended up in CI settings and, through
`worktree login` (decision 6), in a plain-text file. A leaked password gives
all of a user's access, cannot be withdrawn without changing it everywhere,
and every use needs the slow password hash.

## Decision

**Tokens.** A database user (decision 8) can have up to 100 tokens. A token
is `rgpat_` and 32 random bytes in URL-safe base64. Only its SHA-256 is
stored (`worktree_tokens`), with a name, the creation time, an optional
expiry (1 to 3650 days), and the last use, updated at most once a minute
off the request. The token is shown once, when it is created. File users
and identity-service users have no tokens.

**Sign-in.** A token takes the place of the password, with the token's user
name. A password that starts with `rgpat_` is taken for a token, so new
passwords cannot start with it. The digest is looked up directly; a random
token needs no slow hash and no cache. A wrong, expired, or deleted token,
or one for another user name, fails with 401.

**Access.** A token has its user's access, or, with `access`, the lower of
its own and the user's level for every repository (`worktree_token_access`).
A token therefore never has more than its user, also after the user's access
changes.

**API.**

| Request | Answer |
|---|---|
| `POST tokens` | `{name, access?, expires_days?}`; 201 with the token under `token` |
| `GET tokens` | The caller's unexpired tokens |
| `DELETE tokens/<id>` | 204 |
| `GET users/<name>/tokens` | Admins: a user's tokens |
| `DELETE users/<name>/tokens/<id>` | Admins: 204 |

Browser token creation uses the signed-in web session and the same-origin
check used by other changes. Listing and deleting work with a token, so
`logout` can delete its own. `GET me` also answers `token`, the id the caller
signed in with.

**Command line.** `worktree login` checks the credentials, creates a token
named after the computer, and keeps the token instead of the password;
`logout` deletes it. Where the server has no tokens for the user (a file
user, no `[database]`, an older server), the password is kept as before.
`worktree token create | list | delete` manage tokens; `create` asks for
the password.

## Consequences

- Deleting a user deletes its tokens. Changing the password does not.
- Expired tokens stay in the table until the user creates another token,
  but are not listed and do not sign in.
- Log entries record the user name, not the token.
