# 14. Web interface: addresses, sign-in, and read queries

Status: accepted; implemented in `crates/server/src/web/` and `web/`.
Date: October 12, 2026.
`dir`, `session_days`, and `origins` are removed by [decision 15](0015-minimal-configuration.md); customer limits are removed.
Pull request queries and changes, and their pages: [decision 19](0019-pull-requests.md).

## Context

The web interface (decision 13) needs addresses next to Git's, browser
sign-in, and reads of refs, history, trees, and files. Decision 10 removed
the HTTP API for administration; this decision adds no administration,
only reads.

## Decision

**Application.** A single-page application in `web/`: TanStack Start in
SPA mode, TanStack Query for the cache of query answers, and shadcn/ui
components. No server rendering: pages show signed-in, live data, and the
server stays one Rust process. `bun run build` gives
`dist/client`: `_shell.html` and `-/assets/`. The server serves them with
`[web] dir`.

**Addresses.** Everything of the interface lives under `/-/`:
`/-/api/session`, `/-/api/live`, `/-/assets/` (hashed names, cached for a
year), and `/-/sign-in`. Pages of a repository are
`/<name>/-/tree|blob|commits/<rev>/<path>`, `/<name>/-/commit/<oid>`, and
`/<name>/-/branches|tags`; `/<name>` shows the default branch. A segment
`-` is therefore no longer a valid part of a repository name. Every other
`GET` that asks for HTML and is no Git request gets the shell, which routes
in the browser. Git requests are recognized by their path suffixes, as
before, so `git clone https://host/<name>.git` and the page `https://host/<name>`
coexist.

**Sign-in.** `POST /-/api/session {user, password}` checks the Git
password with the identity provider. Personal access tokens are refused:
a session would get the user's full access. The answer sets the session
cookie `worktree_session` (`HttpOnly`, `SameSite=Lax`, `Path=/-/api`
([decision 16](0016-raw-files-and-markdown.md) widens it to `/`),
`Secure` unless the server serves plain HTTP on the loopback address).
`DELETE /-/api/session` removes it.

**Sessions in signed cookies.** The cookie holds the user name, an expiry
(`session_days`, default 14), and an HMAC-SHA256 with `session_secret`
over both and a stamp of the user's stored password. No server state: any
server with the same secret accepts it. Each use (the live connection's
start and every heartbeat) looks the user up by name, so changed access
applies at once, a deleted user is signed out, and a new password ends all
sessions of the user.

**Origins.** Sign-in, sign-out, and the WebSocket upgrade need an `Origin`
that names the requested host, or one of `[web] origins` (for the
development server). Requests without `Origin` come from programs that
hold no cookie of the user and pass.

**Read queries.** Over the live connection (`req`, decision 13): `repos`,
`refs {repo}`, `tree {repo, rev, path}`, `blob {repo, rev, path}`,
`log {repo, rev, path, skip, limit}`, `commit {repo, rev}`. Each needs read
access, counts against the customer's request limit, takes a Git process
slot, and reads a read view, so it sees every push accepted before it and
answers with that generation. The cache also holds objects of rejected
pushes: `rev` is a branch, a tag, or a full ref name, or the object ID of a
commit that a ref reaches (`for-each-ref --contains`); no other revision
syntax. Paths contain no `.` or `..` parts. Files above 1 MiB are shown
without contents, diffs stop at 2 MiB, trees at 5000 entries, and every
Git command at 30 seconds.

## Consequences

- An existing repository whose name has a `-` segment can no longer be
  served; rename it first.
- A stolen cookie works until it expires or the password changes; signing
  out removes only the browser's copy. A table of sessions in PostgreSQL
  can replace the signed cookies when single sessions must be revoked.
- `[identity]` services cannot look users up by name, so web sign-in needs
  users in the configuration file or the database.
- Queries read the local cache of whichever server holds the connection;
  `routing.forward` applies only to Git requests.
- The repository list reads every catalog record per query.
- The configuration file holds the session secret, like the storage keys.
