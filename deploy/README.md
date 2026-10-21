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
- **Secrets.** They stay on the server, outside Dokploy and the repository,
  in `/etc/lager`: `lager.toml`, `ssh_host_ed25519_key`, and
  `postgres.env`. `/etc/lager/set-r2-keys` writes the storage keys into
  `lager.toml` without showing them.
- **Network.** The cloud firewall allows only TCP 22 (Git over SSH, served by
  lager), 80, 443, and Tailscale's UDP 41641. The host's own SSH server is
  disabled; administration uses Tailscale SSH (`ssh root@dokploy`).

Each deployment recreates the lager container, so requests can briefly fail.
Database schema changes apply when the new server starts; there is no
rollback of the schema.

## Self-hosting with compose.yaml

[`compose.yaml`](compose.yaml) runs lager with Caddy, PostgreSQL, and NATS
on one host. The steps below build on that host and switch the running
container. The examples use the SSH alias `lager` and the Compose
directory `/opt/lager`.

### Update with the script

Use a full commit SHA so the same image can also be deployed with the update
script:

```bash
ssh lager "bash -s -- lager:$(git rev-parse HEAD) /opt/lager" < deploy/update.sh
```

Run that command **after** building the image on the host as described below.
It replaces the manual switch and adds health checking and rollback. Do not
run the manual `.env` editing steps at the same time; they do not take the
script's lock.

### 1. Select the revision

Check out the branch or commit you want to deploy and inspect its state:

```bash
git status --short
git log -1 --oneline
```

Build from a clean checkout so the deployment corresponds to a recorded
commit. To update the default branch first, use `git pull --ff-only` from
a checkout of `master`. You can also deploy a committed feature branch.

### 2. Build the image on the server

```bash
deployment_image=lager:$(git rev-parse HEAD)
set -o pipefail
git archive --format=tar HEAD | ssh lager \
  "set -eu
   build_context=\$(mktemp -d /tmp/lager-build.XXXXXXXXXX)
   trap 'rm -rf -- \"\$build_context\"' EXIT
   tar -xf - -C \"\$build_context\"
   DOCKER_BUILDKIT=1 docker build --platform linux/amd64 --tag '$deployment_image' \"\$build_context\" < /dev/null"
```

The `Dockerfile` builds both the server and the web interface using the
server's persistent build cache. Save `$deployment_image` for the next step.
Only committed files are included. Building does not change the running
container.

### 3. Switch the running container

Connect to the deployment host:

```bash
ssh lager
cd /opt/lager
grep '^LAGER_IMAGE=' .env
cp -p .env .env.before-update
```

Edit only `LAGER_IMAGE` in `.env` to the full image name used by
`docker build`, then run:

```bash
docker compose up -d --no-deps lager
docker compose ps lager
docker compose logs --tail 50 lager
```

This recreates the lager container while keeping Caddy, PostgreSQL, and
NATS running. Compose allows up to three minutes and ten seconds for the old server to drain
requests and shut down. Brief connection failures can occur during the
replacement; browser live connections reconnect automatically.

Keep the existing `lager.toml`, database, storage, and Docker volumes.
The image contains the new code and web assets; copying example configuration
over the deployment configuration would discard its settings.

### 4. Verify the deployment

From your local machine:

```bash
curl -fsS https://git.example.com/_lager/health
git ls-remote https://git.example.com/examples/hello.git HEAD
curl -fsS -D - https://git.example.com/examples/hello/-/raw/main/README.md
```

The health endpoint should return `ok`, Git should return the public
repository's HEAD, and the raw route should return the README with
`Content-Type: text/plain; charset=utf-8`.

Open https://git.example.com/examples/hello and check that the connection says
**Live** and the README renders as Markdown. Open `README.md` to check the
Preview/Code toggle and Raw button. For changes involving authentication,
also sign in and check a private repository.

## Roll back the image

The backup made above records the image and configuration from before the
latest update. On the deployment host:

```bash
cd /opt/lager
cp -p .env.before-update .env
docker compose up -d --no-deps lager
docker compose ps lager
docker compose logs --tail 50 lager
```

Repeat the health and browser checks. Keep the previous Docker image until
the update is verified; rollback uses that image without rebuilding it.
