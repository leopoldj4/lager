# rustgit admin

Management interface for a rustgit server: the repository list, and users
with their access levels. It calls the rustgit admin API (`/_rustgit/api/`,
see `../docs/decisions/0004-users-in-storage.md`) from the server side only.

Users can be changed here when the server uses `catalog.kind = "storage"`
and no identity service. Users in `rustgit.toml` are shown read-only.

## Running

Start rustgit (for example `nix run .#local` in the repository root, then
sign in as `dev` / `dev`), then:

```bash
npm install
npm run dev
```

| Variable | Default | Purpose |
|---|---|---|
| `RUSTGIT_URL` | `http://127.0.0.1:8080` | Where this app reaches rustgit |
| `RUSTGIT_PUBLIC_URL` | `RUSTGIT_URL` | Base of the clone URLs shown |
| `SESSION_SECRET` | random per start in development | Encrypts the session cookie; required in production |

The session cookie holds the signed-in user's rustgit credentials,
encrypted with AES-256-GCM, httpOnly, for eight hours: the API checks them
on every request. Use HTTPS in front of `npm start`.
