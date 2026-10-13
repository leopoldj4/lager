# Updating the deployment

The current deployment serves **https://git.leoj.de**. SSH connects as
`root` through the `worktree` alias, and its Docker Compose files are in
`/opt/worktree`. Run the build and transfer commands from a Linux checkout
with Nix and access to that SSH alias.

## 1. Select the revision

Check out the branch or commit you want to deploy and inspect its state:

```bash
git status --short
git log -1 --oneline
```

Build from a clean checkout so the deployment corresponds to a recorded
commit. To update the default branch first, use `git pull --ff-only` from
a checkout of `master`. You can also deploy a committed feature branch.

## 2. Build and transfer the image

```bash
deployment_image_stream=$(nix build .#docker --no-link --print-out-paths)
set -o pipefail
"$deployment_image_stream" | ssh worktree 'docker load'
```

Nix builds both the server and the web interface. The final output prints
`Loaded image: worktree:<tag>`. Save that full image name for the next step.
Loading the image does not change the running deployment.

## 3. Switch the running container

Connect to the deployment host:

```bash
ssh worktree
cd /opt/worktree
grep '^WORKTREE_IMAGE=' .env
cp -p .env .env.before-update
```

Edit only `WORKTREE_IMAGE` in `.env` to the full image name printed by
`docker load`, then run:

```bash
docker compose up -d --no-deps worktree
docker compose ps worktree
docker compose logs --tail 50 worktree
```

This recreates the worktree container while keeping Caddy, PostgreSQL, and
NATS running. Compose allows up to two minutes for the old server to drain
requests and shut down. Brief connection failures can occur during the
replacement; browser live connections reconnect automatically.

Keep the existing `worktree.toml`, database, storage, and Docker volumes.
The image contains the new code and web assets; copying example configuration
over the deployment configuration would discard its settings.

## 4. Verify the deployment

From your local machine:

```bash
curl -fsS https://git.leoj.de/_worktree/health
git ls-remote https://git.leoj.de/examples/hello.git HEAD
curl -fsS -D - https://git.leoj.de/examples/hello/-/raw/main/README.md
```

The health endpoint should return `ok`, Git should return the public
repository's HEAD, and the raw route should return the README with
`Content-Type: text/plain; charset=utf-8`.

Open https://git.leoj.de/examples/hello and check that the connection says
**Live** and the README renders as Markdown. Open `README.md` to check the
Preview/Code toggle and Raw button. For changes involving authentication,
also sign in and check a private repository. The Markdown deployment changed
the session cookie path, so existing users may need to sign in again.

## Roll back the image

The backup made above records the image and configuration from before the
latest update. On the deployment host:

```bash
cd /opt/worktree
cp -p .env.before-update .env
docker compose up -d --no-deps worktree
docker compose ps worktree
docker compose logs --tail 50 worktree
```

Repeat the health and browser checks. Keep the previous Docker image until
the update is verified; rollback uses that image without rebuilding it.
