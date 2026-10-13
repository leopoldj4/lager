# worktree

A Git server in Rust that keeps repositories in an S3-compatible storage
service. Clients use standard Git over smart HTTP. Any number of servers
share one storage service; each has a local cache that it can rebuild at
any time. A head object changed only with compare-and-swap orders all
changes of a repository.

- [implementation plan](IMPLEMENTATION_PLAN.md)
- [Decisions](docs/decisions/) made during implementation

## Local start

One server, a local S3-compatible service (SeaweedFS), PostgreSQL for
users, NATS for change events, and a development user. Storage data, the database,
and caches live in separate directories under `./.local`; stored data is
kept between runs.

```bash
nix run .#local
```

```bash
git clone http://dev:dev-password@127.0.0.1:8080/demo.git
```

The web interface is at http://127.0.0.1:8080/ (sign in as `dev` / `dev-password`).
A `.local/worktree.toml` from before the minimal configuration
([decision 15](docs/decisions/0015-minimal-configuration.md)) no longer
loads; remove it and the script writes a new one.

The script is defined in `flake.nix` (`packages.local`). Git 2.45 or later
is required.

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
```

Every server is plain HTTP behind a load balancer that terminates HTTPS
and spreads requests over the servers. Any server can answer any request;
it fills its cache for the repository on first use. The server that
accepts a push creates the next automatic snapshot (every 200 entries).
The web interface is served from `share/worktree/web` beside the programs
(the Nix package and the container image include it); without it, only the
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
  (`/-/api/live`) carries queries and change events. Repository names
  cannot contain a `-` segment, which the interface reserves. See
  [decision 13](docs/decisions/0013-live-connection-protocol.md) and
  [decision 14](docs/decisions/0014-web-interface.md).
- Every response has an `x-worktree-request-id` header. A push uses the same
  UUID as its request UUID in the log entry and in the client's
  `remote: worktree: accepted ...` line. Logs never contain credentials or
  repository contents.
- If the storage service is unavailable, new reads and writes fail with 503.
  A push whose result is unknown says so; the client should fetch before it
  pushes again.

## Container image

`nix build .#docker` (Linux, or a Linux builder) gives a script that
streams the image:

```bash
nix build .#docker && ./result | docker load
```

The image holds worktree (`/bin/worktree-server` and `/bin/worktree`), the
built web interface (`/share/worktree/web`), the pinned Git, tini as PID 1,
and CA certificates. It runs `worktree-server` as user 10001 and reads
`/etc/worktree/worktree.toml`; mount the configuration as a secret, and the
cache at `/var/cache/worktree` on local disk. Set the container stop timeout
above 185 seconds, so that running pushes finish.

`deploy/` runs it on one host with Docker Compose: Caddy for HTTPS, worktree,
NATS, and PostgreSQL, with repositories in an external S3-compatible service.

```bash
nix build .#docker && ./result | docker load      # note the image name
cd deploy
cp .env.example .env                               # domain, image, database password
cp worktree.example.toml worktree.toml             # storage keys, session secret
docker compose up -d
docker compose exec worktree worktree user create <name> --access '*=force'
docker compose exec worktree worktree token create <name> laptop
```

On a Mac, an OrbStack NixOS machine works as the Linux builder: it sees
the Mac's files, so this builds and loads the image into OrbStack's Docker:

```bash
orb create nixos nix-builder
orb -m nix-builder nix --extra-experimental-features "nix-command flakes" build "$PWD#docker" --no-link --print-out-paths | xargs -I{} orb -m nix-builder {} | docker load
```

That builds for the Mac's architecture (arm64). For x86_64 servers, name
the system: the same machine builds it with Rosetta. Only worktree is compiled; everything else comes from the binary
cache. The first build takes about ten minutes.

```bash
orb -m nix-builder nix --extra-experimental-features "nix-command flakes" build "$PWD#packages.x86_64-linux.docker" --no-link --print-out-paths | xargs -I{} orb -m nix-builder {} | docker load
```

## Development

```bash
nix develop
cargo build
nix flake check    # build, clippy, and rustfmt
```

The web interface lives in `web/` (TanStack Start in SPA mode, shadcn/ui).
With a server on 127.0.0.1:8080 (`nix run .#local`):

```bash
cd web && npm install
npm run dev        # http://localhost:3000, API and WebSocket go to the server
npm run build      # dist/client, what the server serves
npm run typecheck
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
