# 17. Passkey login

Status: accepted.

Accounts can register up to 20 WebAuthn passkeys for browser login. Passwords
remain available for recovery and Git; personal access tokens cannot enroll
passkeys. Enrollment requires both a valid browser session and password
confirmation. A passkey authenticates the account with its current access
levels through the existing signed session cookie.

`web.passkey_origin` explicitly names the browser-facing origin. The RP ID
is its hostname. The origin must be HTTPS, with an exception for HTTP on
localhost during development. It has no path or trailing slash. Omitting
the setting disables ceremonies. This extends the minimal configuration in
decision 15 because the internal listen address cannot identify the public
HTTPS origin behind a proxy.

`webauthn-rs` verifies registration and authentication, including signatures,
RP ID, origin, challenge, and user verification. Public credentials are
stored with an opaque, stable account UUID and cascade on account deletion.
Authentication starts with the user name and offers that account's keys;
discoverable, usernameless login is outside this change.

Five-minute ceremony state stays in PostgreSQL. Only challenge options
reach JavaScript; a separate random, HttpOnly, SameSite=Strict cookie binds
the ceremony to the browser. Every ceremony endpoint checks the exact
configured Origin. A DELETE RETURNING atomically consumes state before
verification, even if the proof fails. State is bounded to five ceremonies
per user, and expired state is cleaned on subsequent starts. Any server can
finish a ceremony started on another server.

Password stamps bind pending ceremonies to the account version. Credential
updates use a compare-and-swap against the stored public credential, so
revocation and concurrent counter changes cannot be bypassed with stale
ceremony state. Removing a passkey blocks future authentication, but does
not terminate issued sessions. Password changes end pending ceremonies and
sessions while preserving enrolled passkeys.
