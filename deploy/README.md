# Deployment

The current deployment serves **https://git.leoj.de**. SSH connects as
`root` through the `worktree` alias, and its Docker Compose files are in
`/opt/worktree`. Run the manual commands from a Git checkout with access to that SSH alias.
Docker builds run on the deployment host.

## Automatic deployment

[`.github/workflows/deploy.yaml`](../.github/workflows/deploy.yaml) deploys
pushes to **`master`**, the current GitHub default branch, and supports a
manual **Run workflow** on that branch. If the default branch is renamed to
`main`, update both the push filter and the job's branch guard.

The GitHub-hosted runner checks out the commit, joins Tailscale temporarily,
and streams `git archive` into a temporary directory on the production host.
Docker builds from that directory, avoiding BuildKit stdin-context session
errors when loading the Dockerfile frontend. The directory is removed when
the remote command exits, including on build failure. Only the committed
source is transferred; no Git credentials or persistent checkout are needed
on the server. The host builds the existing Dockerfile for Linux
amd64, including the Rust server and web assets, tags it `worktree:<full SHA>`,
then the runner invokes [`update.sh`](update.sh) over SSH.

BuildKit on the server retains Docker layers and the Dockerfile's Cargo/Bun
cache mounts between builds. No image transfer or registry login is needed;
the existing Vultr registry secrets are unused. The build shares CPU, RAM,
and disk with the running application; leave enough capacity for compilation.
The running app stays in place until the build succeeds.

The update script locks deployments on the host, backs up `.env` to
`.env.before-update`, changes only `WORKTREE_IMAGE`, and recreates only the
worktree service. It checks the new container's health directly for up to
about 150 seconds and restores the previous image if startup or health fails.
The workflow then checks the public HTTPS endpoint; failure of that external
check reports a failed workflow but does not roll back an internally healthy
container. The old image is retained for manual rollback. Image rollback does
not undo database migrations; schema changes must remain compatible with the
previous release.

Deployments run one at a time and are not canceled by newer pushes. GitHub
may replace a pending run with a newer pending run. This is still a single
app container: replacement can briefly interrupt requests. Configuration,
Compose, Caddy, PostgreSQL, NATS, and storage remain managed on the host;
changes to their checked-in examples are not automatically installed.

### One-time activation

1. In the Tailscale policy, define `tag:worktree-ci` and
   `tag:worktree-production`, and assign the production tag to the `worktree`
   host. Preserve the existing administrator access rules when tagging the
   host (tagging changes its identity). Merge rules equivalent to these into
   the existing policy:

   ```json
   {
     "tagOwners": {
       "tag:worktree-ci": ["autogroup:admin"],
       "tag:worktree-production": ["autogroup:admin"]
     },
     "grants": [{
       "src": ["tag:worktree-ci"],
       "dst": ["tag:worktree-production"],
       "ip": ["tcp:22"]
     }],
     "ssh": [{
       "action": "accept",
       "src": ["tag:worktree-ci"],
       "dst": ["tag:worktree-production"],
       "users": ["root"]
     }]
   }
   ```

   Tailscale SSH must be enabled on the host (`tailscale set --ssh`); it
   already is on the current deployment. CI needs a noninteractive `accept`
   rule, rather than an interactive `check` rule. Tagged CI nodes need a
   tagged destination for Tailscale SSH. No SSH private key is stored in
   GitHub; `tailscale ssh` verifies the host through Tailscale.

2. Create a Tailscale OAuth client with **Auth Keys: Write** for
   `tag:worktree-ci`. Add its credentials as GitHub Actions secrets
   `TS_OAUTH_CLIENT_ID` and `TS_OAUTH_SECRET` in the repository or the
   `production` environment. See the
   [Tailscale GitHub Action setup](https://tailscale.com/docs/integrations/github/github-action).
   The runner identity can run commands as root on this deployment host;
   restrict the OAuth client to this tag.

3. Configure the GitHub `production` environment to allow only `master`.
   Leave required reviewers off if pushes should deploy without approval.
   The workflow also rejects manual runs on other branches. Protect `master`
   and review changes to the workflow and deployment script.

4. Confirm `/opt/worktree` has the existing `.env`, `compose.yaml`, and
   `worktree.toml`, a working app image, Docker with Buildx/BuildKit, Docker Compose, Bash,
   Python 3, `flock`, and `curl`. The workflow targets
   `root@worktree.tailb80a2d.ts.net`; change `DEPLOY_HOST` if that name changes.
   Merge the workflow into `master`, then inspect its first run in Actions.
   A build failure leaves production untouched.

## Manual deployment

The steps below remain available without GitHub Actions. Use a full commit
SHA so the same image can also be deployed with the update script:

```bash
ssh worktree "bash -s -- worktree:$(git rev-parse HEAD) /opt/worktree" < deploy/update.sh
```

Run that command **after** building the image on the host as described below.
It replaces the manual switch and adds health checking and rollback. Do not
run the manual `.env` editing steps concurrently with an automated deployment;
they do not take the script's lock.

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
deployment_image=worktree:$(git rev-parse HEAD)
set -o pipefail
git archive --format=tar HEAD | ssh worktree \
  "set -eu
   build_context=\$(mktemp -d /tmp/worktree-build.XXXXXXXXXX)
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
ssh worktree
cd /opt/worktree
grep '^WORKTREE_IMAGE=' .env
cp -p .env .env.before-update
```

Edit only `WORKTREE_IMAGE` in `.env` to the full image name used by
`docker build`, then run:

```bash
docker compose up -d --no-deps worktree
docker compose ps worktree
docker compose logs --tail 50 worktree
```

This recreates the worktree container while keeping Caddy, PostgreSQL, and
NATS running. Compose allows up to three minutes and ten seconds for the old server to drain
requests and shut down. Brief connection failures can occur during the
replacement; browser live connections reconnect automatically.

Keep the existing `worktree.toml`, database, storage, and Docker volumes.
The image contains the new code and web assets; copying example configuration
over the deployment configuration would discard its settings.

### 4. Verify the deployment

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
