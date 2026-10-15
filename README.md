# worktree

A Git server in Rust that keeps repositories in an S3-compatible storage
service. Clients use standard Git over smart HTTP. Any number of servers
share one storage service; each has a local cache that it can rebuild at
any time. A head object changed only with compare-and-swap orders all
changes of a repository.

- [implementation plan](IMPLEMENTATION_PLAN.md)
- [Decisions](docs/decisions/) made during implementation

## Local start

One server built from this checkout, a local S3-compatible service
(SeaweedFS), PostgreSQL for users, NATS for change events, and a
development user. SeaweedFS, PostgreSQL, and NATS run in Docker; stored
data is kept between runs in Docker volumes. The configuration and the
cache live under `./.local`.

```bash
scripts/local.sh
```

```bash
git clone http://dev:dev-password@127.0.0.1:8080/dev/demo.git
```

The web interface is at http://127.0.0.1:8080/ (sign in as `dev` / `dev-password`).
A `.local/worktree.toml` from before the minimal configuration
([decision 15](docs/decisions/0015-minimal-configuration.md)) no longer
loads; remove it and the script writes a new one.

For GitHub mirrors, register a GitHub OAuth app with the callback URL
`http://127.0.0.1:8080/-/api/github/callback` and start with
`WORKTREE_GITHUB_CLIENT_ID=... WORKTREE_GITHUB_CLIENT_SECRET=... scripts/local.sh`.

It needs Rust (rustup reads `rust-toolchain.toml`), the OpenSSL headers
(`libssl-dev`), Bun, Docker with Compose, and Git 2.45 or later.
`docker compose -p worktree-local down -v` removes the stored data.

### Demo data

`scripts/seed.ts` creates two users, `demo` and `alex` (password
`demo-password`), and repositories under `demo/` to try everything out:
a public website with branches, tags, a binary file, and six pull requests
(open, conflicting, merged, squashed, closed, merged by a push), a private
API with a long history, a large diff, and ref rules, a nested repository
with the default branch `trunk`, a private one only `demo` reads, and an
empty one. `alex` has `write` on the website and `read` on the others.

```bash
bun scripts/seed.ts                                            # the local server
bun scripts/seed.ts --url https://git.example.com --ssh worktree # a deployment
```

`--ssh` runs the administration commands in the Compose deployment on that
host (`--compose-dir`, default `/opt/worktree`); `--cli "<command>"` names
any other. Repositories that exist already are skipped; `--reset` deletes
and seeds them again. `--user`, `--reviewer`, and `--password` change the
names and the password.

## Commands

The server and the administration command line are separate programs:

```text
worktree-server [-c worktree.toml]
```

```text
worktree user create <name> [--access repo=level]...   # asks for the password
worktree user list
worktree user set-password <name>
worktree user set-access <name> [repo=level]...       # replaces all levels
worktree user delete <name>
worktree token create <user> <name> [--access repo=level]... [--expires-days N]
worktree token list <user>
worktree token delete <user> <id>
worktree repo create <name> [--default-branch main]
worktree repo delete <name>
worktree repo set-default-branch <name> <branch>
worktree repo set-public <name> true|false   # read without signing in
worktree repo set-ref-rule <name> <pattern> [--update L] [--force L] [--delete L]
worktree repo delete-ref-rule <name> <pattern>
worktree repo show <name>
worktree repo list
worktree cleanup <name> [--delete]            # report, or remove, obsolete data
worktree backup export <name> <dir>
worktree backup import <dir> [--name <name>]
```

Both programs read the server configuration (`-c`, `WORKTREE_CONFIG`, or
`./worktree.toml`). `repo`, `cleanup`, and `backup` use the
storage service directly; `user` and `token` use the database.
The server has no management API
([decision 10](docs/decisions/0010-no-admin-api.md)).

Users and their access levels live only in PostgreSQL and apply to the
next request on every server. The server creates and updates its tables
when it starts. See [decision 8](docs/decisions/0008-users-in-postgres.md).
Access levels are `none`, `read`, `write`, and `force` (also
non-fast-forward updates and tag moves), per repository name or `*`.
Every user has `force` on their own repositories (`<user>/...`), which also
lets them create one with a push; a token limited to other repositories
does not reach them.

Repository names start with the owning user's name and have at least one
more segment: `alice/app` or `alice/tools/app`, but not `app` or `alice`
([decision 18](docs/decisions/0018-repositories-under-users.md)).

Repositories live only in the catalog in storage
(`<prefix>catalog/<sha256 of the name>.json`): the name, the repository
UUID, the default branch, whether anyone may read it without signing in,
and its ref rules. Changes apply within five seconds on every server. A
`git push` to a missing name creates the repository when the user has
`write` access to the name; the first push also sets the default branch if
it does not contain `main`. See
[decision 3](docs/decisions/0003-catalog-identity-and-ref-rules.md) and
[decision 5](docs/decisions/0005-create-on-push.md).

Ref rules set the levels that changes to matching refs need. A pattern is a
ref name or a prefix ending in `*`; the most specific one applies:

```bash
worktree repo set-ref-rule org/app refs/heads/main --delete force
worktree repo set-ref-rule org/app 'refs/heads/release/*' --update force
```

Personal access tokens (`rgpat_…`) work in place of a user's
password: `git clone https://alice:rgpat_…@git.example.com/org/app.git`.
`worktree token create` prints one once, optionally limited to some
repositories (`--access org/app=read`) and with an expiry. A token never has
more access than its user. See
[decision 9](docs/decisions/0009-personal-access-tokens.md).

`cleanup` reports only, unless `--delete` is given. Run the report first
and review it. Packs and records have separate retention periods
(`--pack-retention-hours`, `--record-retention-hours`); see
[decision 2](docs/decisions/0002-data-removal-fence.md).

## Configuration

The file holds only what a server needs to start
([decision 15](docs/decisions/0015-minimal-configuration.md)). Limits and
timings are fixed in `crates/core/src/limits.rs`.

```toml
[server]
listen = "0.0.0.0:8080"            # plain HTTP; a load balancer terminates HTTPS
# cache_dir = "/var/cache/worktree"  # the default; local SSD, can be removed at any time

[storage]                          # S3-compatible, with conditional writes
endpoint = "https://s3.example.com"  # leave out for AWS S3; http:// for local development
region = "eu-central-1"            # default us-east-1
bucket = "git-repositories"
access_key_id = "..."
secret_access_key = "..."          # restrict to the prefix
prefix = "prod"                    # key prefix in the bucket; default none

[database]                         # users and access tokens
url = "postgres://worktree:...@db.example.com/worktree?sslmode=verify-full"

[bus]                              # change events between servers (NATS)
url = "nats://nats.internal:4222"  # without [bus], events stay on this server

[web]                              # web interface and its API under /-/
session_secret = "..."             # at least 32 characters, the same on every server
public_origin = "https://git.example.com" # optional; exact public origin, no trailing slash

[github]                           # optional; a GitHub OAuth app for mirrors
client_id = "..."                  # callback URL: https://git.example.com/-/api/github/callback
client_secret = "..."
```

Every server is plain HTTP behind a load balancer that terminates HTTPS
and spreads requests over the servers. Any server can answer any request;
it fills its cache for the repository on first use. The server that
accepts a push creates the next automatic snapshot (every 200 entries).
The web interface is served from `share/worktree/web` beside the programs
(the container image includes it; `scripts/local.sh` links it into
`target/`); without it, only the
API under `/-/api/` answers.

## Operation

- `GET /_worktree/health`: the load balancer check. 503 while the storage
  service does not answer (checked at most once per second), while the NATS
  connection of `[bus]` is down for longer than 5 seconds or has
  not been made yet, or while the server stops. `GET /_worktree/live` only shows that the process runs.
- On `SIGTERM` or `SIGINT` the server stops cleanly: the health check fails
  for 5 seconds while requests are still served, then the server
  accepts no new connections and waits up to 180 seconds for
  running requests and pushes. Responses meanwhile carry
  `x-worktree-draining: 1`. A
  second signal stops at once, which is also safe. Allow the service
  manager enough time (for systemd, `TimeoutStopSec` above 185 seconds).
- `GET /_worktree/metrics`: counters and median, 95th, and 99th percentile
  delays per push step, in Prometheus text format. It needs no sign-in;
  keep it away from the public network.
- After an accepted push, the server publishes the repository and generation
  on NATS; other servers then update their caches. Lost events are harmless:
  every request checks the storage head. See
  [decision 12](docs/decisions/0012-nats-change-bus.md).
- The web interface (`[web]`) shows repositories, files, history, and
  commits, and updates open pages on every push. Browsers sign in with the
  Git password and keep a signed session cookie; one WebSocket per tab
  (`/-/api/live`) carries queries and change events, and
  `/<name>/-/raw/<rev>/<path>` serves a file's bytes. Markdown files and
  READMEs are rendered. Repository names cannot contain a `-` segment,
  which the interface reserves. See
  [decision 13](docs/decisions/0013-live-connection-protocol.md),
  [decision 14](docs/decisions/0014-web-interface.md), and
  [decision 16](docs/decisions/0016-raw-files-and-markdown.md).
- Pull requests: open one on the web from two branches of a repository
  (`/<repo>/-/pulls`), discuss it, and merge it with a merge commit or as
  one squashed commit. Users with read access open and comment; merging
  needs write access and the ref rules of the target branch. The server
  keeps `refs/pull/<n>/head` at the source branch (`git fetch origin
  pull/7/head`); clients cannot push to `refs/pull/`. Pull requests live in
  PostgreSQL, their refs and merges in the log; a push that brings the
  source into the target marks the pull request merged. See
  [decision 19](docs/decisions/0019-pull-requests.md).
- GitHub mirrors (`[github]`): **Settings** in the user menu has
  **Connect with GitHub**. Then pick GitHub repositories; each becomes
  `<user>/<name>` here and follows GitHub's branches, tags, and default
  branch within about a minute. Mirrors are read-only: pushes and pull
  requests are refused. **Stop syncing** keeps the repository and makes it
  writable. See [decision 20](docs/decisions/0020-github-mirrors.md).
- Every response has an `x-worktree-request-id` header. A push uses the same
  UUID as its request UUID in the log entry and in the client's
  `remote: worktree: accepted ...` line. Logs never contain credentials or
  repository contents.
- If the storage service is unavailable, new reads and writes fail with 503.
  A push whose result is unknown says so; the client should fetch before it
  pushes again.

## Container image

Build it from the `Dockerfile`, tagged with the commit:

```bash
docker build -t worktree:$(git rev-parse --short HEAD) .
```

The image holds worktree (`/usr/local/bin/worktree-server` and
`/usr/local/bin/worktree`), the
built web interface (`/usr/local/share/worktree/web`), Git from Debian
trixie, tini as PID 1,
and CA certificates. It runs `worktree-server` as user 10001 and reads
`/etc/worktree/worktree.toml`; mount the configuration as a secret, and the
cache at `/var/cache/worktree` on local disk. Set the container stop timeout
above 185 seconds, so that running pushes finish.

`deploy/` runs it on one host with Docker Compose: Caddy for HTTPS, worktree,
NATS, and PostgreSQL, with repositories in an external S3-compatible service.

For updates to an existing deployment, see the [deployment update guide](deploy/README.md).

```bash
docker build -t worktree:$(git rev-parse --short HEAD) .   # note the image name
cd deploy
cp .env.example .env                               # domain, image, database password
cp worktree.example.toml worktree.toml             # storage keys, session secret
docker compose up -d
docker compose exec worktree worktree user create <name> --access '*=force'
docker compose exec worktree worktree token create <name> laptop
```

To run storage on the same host instead (for example SeaweedFS on the
Docker bridge, 172.17.0.1), add a `compose.override.yaml` that gives the
server a name for the host, set `endpoint = "http://s3:8333"`, and let the
containers through the host firewall:

```yaml
services:
  worktree:
    extra_hosts:
      - "s3:host-gateway"
```

```bash
ufw allow from 172.16.0.0/12 to 172.17.0.1 port 8333 proto tcp
```

On a Mac, build for x86_64 servers with
`docker build --platform linux/amd64 ...`; it runs under emulation and is
slow.

## Development

```bash
cargo build
cargo clippy --all-targets -- --deny warnings
cargo fmt --check
```

To share compiled dependencies between checkouts on one machine, install
[sccache](https://github.com/mozilla/sccache) and set it as the compiler
wrapper in `~/.cargo/config.toml`:

```toml
[build]
rustc-wrapper = "sccache"
```

`sccache --show-stats` shows the hit rate.

The web interface lives in `web/` (TanStack Start in SPA mode, shadcn/ui).
With a server on 127.0.0.1:8080 (`scripts/local.sh`):

```bash
cd web && bun install
bun run dev        # http://localhost:3000, API and WebSocket go to the server
bun run build      # dist/client, what the server serves
bun run typecheck
```

The Cargo workspace has three crates:

| Crate | Contents |
|---|---|
| `crates/core` (`worktree-core`) | Configuration, fixed limits, record formats, storage, catalog, identities, repository creation and deletion, history reading, data removal, and backup. |
| `crates/server` (`worktree-server`) | The Git process wrapper, the repository engine (caches and pushes), snapshots, Git HTTP requests, access checks, limits, change events, the web API and live connection, server assembly, and the `proc-receive` hook program. |
| `web` | The web interface. The server serves its built files. |
| `crates/cli` (`worktree-cli`, binary `worktree`) | The administration commands. It does not depend on the server crate. |

## License

[Functional Source License 1.1, Apache 2.0 Future License](LICENSE.md)
(`FSL-1.1-ALv2`). You may use, change, and redistribute the code for any
purpose other than a product or service that competes with it. Each version
becomes available under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
two years after it is released.

### Passkey login

Set `web.public_origin` to the exact public origin (for example
`https://git.example.com`) on every server and restart. HTTPS is required;
`http://localhost:8080` is allowed for local development. For the Vite UI,
use its origin instead, for example `http://localhost:3000`, and open that
same address in the browser. The RP ID is the origin's hostname; changing
that hostname requires enrolling new passkeys.

Sign in, open **Passkeys** in the user menu, then name
and add a passkey. On later visits, choose
**Sign in with a passkey** and select your saved account. No user name is needed. The same account page
lists and removes passkeys (up to 20 per account). Password and Git token
login remain available. Removing a passkey blocks future logins with it;
it does not revoke existing sessions. Changing the account password ends
sessions and pending ceremonies, but enrolled passkeys remain valid.

Schema migrations run at startup. Public credentials and five-minute,
single-use ceremony state live in PostgreSQL, so no sticky sessions are
needed. The serialization feature of
[`webauthn-rs`](https://docs.rs/webauthn-rs/0.5.5/webauthn_rs/)
is used only for server-side state, never browser storage.
