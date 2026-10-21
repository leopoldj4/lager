# 23. Passkey sign-up

Status: accepted; implemented in `crates/core/src/identity/passkeys.rs` and
`crates/server/src/web/passkeys.rs`. Date: October 20, 2026.

## Context

Only an administrator could create users, with `lager user create`. People
should be able to create their own account on the website. Passkeys
(decision 17) already sign in without a user name or password, so a new
account needs neither a password nor an administrator.

## Decision

`web.sign_up = true` lets anyone create an account; it needs
`web.public_origin`. `GET /-/api/passkeys/signup` tells the sign-in page
whether to offer it. `POST /-/api/passkeys/signup/start` takes a user name
and starts a registration ceremony; `.../finish` verifies the passkey,
creates the account, and signs it in.

**Names.** A name follows the user name rules. It is taken if a user has it
in any case (`Alice` blocks `alice`), or if any catalog record, even a
deleted one, is named under it: repositories under a name belong to whoever
has it (decision 18). Both are checked at the start, before the browser saves
a passkey, and again at the finish.

**Ceremony.** Sign-up is an anonymous ceremony: it counts toward the bound of
1024 and replaces the browser's previous one, like a login. The account's
passkey user handle is chosen at the start, so the authenticator stores the
handle the account will have. Nothing is written to `lager_users` until
the passkey is verified. The finish holds the anonymous-ceremony lock while it
checks the name and inserts the user and the passkey in one transaction.

**No password.** The stored hash is `!no-password$` and 32 random bytes. No
password hash parses it, so password and Git Basic sign-in fail; the random
rest gives each account its own session stamp. An administrator can set a
password with `lager user`, which ends its sessions as usual. Git uses
access tokens or SSH keys created in the settings.

**Last passkey.** An account without a password cannot remove its last
passkey; the request answers 409.

New accounts have no access levels: they own `<name>/...` (decision 18) and
nothing else.

## Consequences

- Anyone who reaches the server can create accounts and repositories under
  them. Only the ceremony bound limits the rate; put limits in front of the
  server if that matters.
- Each start and finish lists the catalog to check names.
- A sign-up that loses a race for its name leaves an unused passkey in the
  authenticator.
- Losing every passkey of an account without a password needs an
  administrator to set a password.
