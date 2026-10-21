# 25. Password sign-up

Status: accepted; implemented in `crates/server/src/web/sign_up.rs` and
`crates/core/src/identity/passkeys.rs`. Date: October 21, 2026.

## Context

Sign-up worked only with a passkey (decision 23), so it needed a public
HTTPS origin and a browser and device that support passkeys. A password
works everywhere, and Git can use it directly over HTTPS.

## Decision

`LAGER_SIGN_UP=true` offers sign-up with a user name and a password, with or
without `LAGER_DOMAIN`; passkey sign-up is offered as well when the domain
is set. `GET /-/api/signup` answers `{"enabled", "passkey"}` for the pages.
`POST /-/api/signup` takes `{"user", "password"}`, creates the account, and
signs it in.

- The rules are those of passkey sign-up and `lager user create`: the user
  name rules, the password rules (12 characters or more, not starting with
  the token prefix), and a name is taken if a user has it in any case or a
  catalog record, even a deleted one, is named under it.
- The account is inserted under the same lock as passkey sign-up, so two
  sign-ups for names that differ only in case cannot both succeed.
- The password is hashed as by `lager user create`, off the request threads.
- Like a password sign-in, the request needs an allowed `Origin`, if any.
- New accounts have no access levels: they own `<name>/...` (decision 18).

## Consequences

- Anyone who reaches the server can create accounts. Nothing limits the
  rate, and each sign-up costs one slow password hash, as a failed sign-in
  does; put limits in front of the server if that matters.
- Such accounts can add passkeys later in the settings, like any other.
