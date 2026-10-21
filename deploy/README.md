# Deployment

## Staging: staging.lager.sh

Every push to **`master`** deploys to **https://staging.lager.sh**, a
Dokploy server (`dokploy` on Tailscale) that builds the image itself from
[`dokploy.compose.yaml`](dokploy.compose.yaml). Nothing runs in GitHub
Actions.

- **Trigger.** Dokploy's GitHub App sends push events to Dokploy. The
  dashboard is reachable only over Tailscale; Tailscale Funnel exposes only
  the webhook path, `https://dokploy.tailb80a2d.ts.net/api/deploy/github`,
  and Dokploy rejects requests without the app's signature. Commits whose
  message contains `[skip ci]` are not deployed.
- **Build.** Dokploy clones the commit and runs `docker compose up -d --build`
  with the repository's `Dockerfile`. The build shares CPU and memory with the
  running server, which stays in place until the build succeeds.
- **HTTPS.** Dokploy's Traefik terminates HTTPS with a Let's Encrypt
  certificate and hides `/_lager/metrics`. Its entry points must not time
  out long pushes: `transport.respondingTimeouts.readTimeout` is `0s` for
  `web` and `websecure` in Dokploy's Traefik settings.
- **Configuration.** Dokploy's environment for the service holds it, secrets
  included; `dokploy.compose.yaml` passes it to the containers. See below.
- **Network.** The cloud firewall allows only TCP 22 and 2222 (Git over SSH,
  served by lager: production on 22, staging on 2222), 80, 443, and
  Tailscale's UDP 41641. The host's own SSH server is
  disabled; administration uses Tailscale SSH (`ssh root@dokploy`).

Each deployment recreates the lager container, so requests can briefly fail.
Database schema changes apply when the new server starts; there is no
rollback of the schema.

## Environment

Set these in the Dokploy service's **Environment**. lager's variables are
listed in the [README](../README.md#configuration); the compose file sets
`LAGER_DATABASE_URL` and `LAGER_NATS_URL` itself.

```bash
LAGER_DOMAIN=staging.lager.sh
LAGER_S3_ENDPOINT=https://<account>.r2.cloudflarestorage.com
LAGER_S3_REGION=auto
LAGER_S3_BUCKET=...
LAGER_S3_ACCESS_KEY_ID=...
LAGER_S3_SECRET_ACCESS_KEY=...
LAGER_S3_PREFIX=lager
LAGER_SESSION_SECRET=...      # openssl rand -hex 32
LAGER_SSH_HOST_KEY="-----BEGIN OPENSSH PRIVATE KEY-----
...
-----END OPENSSH PRIVATE KEY-----"
POSTGRES_PASSWORD=...         # openssl rand -hex 32; goes into a URL, so hex only
LAGER_SSH_PORT=2222           # staging; production leaves it out and gets 22
# Optional:
# LAGER_SIGN_UP=true
# LAGER_GITHUB_CLIENT_ID=...
# LAGER_GITHUB_CLIENT_SECRET=...
```

PostgreSQL takes `POSTGRES_PASSWORD` only when it creates the database. To
change it later, change it in the database first
(`ALTER USER lager PASSWORD '...'`), then in Dokploy.

For a new SSH host key: `ssh-keygen -t ed25519 -N '' -C lager -f key`, and
paste the contents of `key`. Keep it: clients refuse a changed host key.
