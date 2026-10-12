# worktree

A Git server in Rust that keeps repositories in an S3-compatible storage
service. Clients use standard Git over smart HTTP. Any number of servers
share one storage service; each has a local cache that it can rebuild at
any time. A head object changed only with compare-and-swap orders all
changes of a repository.

- [Design](ARCHITECTURE.md) and [implementation plan](IMPLEMENTATION_PLAN.md)
- [Decisions](docs/decisions/) made during implementation

## Local start

One server, a local S3-compatible service (SeaweedFS), PostgreSQL for
managed users, NATS for change events, and development credentials. Storage data, the database,
and caches live in separate directories under `./.local`; stored data is
kept between runs.

```bash
nix run .#local
```

```bash
git clone http://dev:dev@127.0.0.1:8080/demo.git
```

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
worktree repo list
worktree cleanup <name> [--delete]            # report, or remove, obsolete data
worktree backup export <name> <dir>
worktree backup import <dir> [--name <name>]
worktree hash-password < password.txt         # for password_hash
```

Both programs read the server configuration (`-c`, `WORKTREE_CONFIG`, or
`./worktree.toml`). `repo`, `cleanup`, and `backup` use the
storage service directly; `user` and `token` use the database
(`[database]`). The server has no management API
([decision 10](docs/decisions/0010-no-admin-api.md)).

Users in `[database]` need no configuration change and apply to the next
request on every server; users in the file take precedence. The server
creates and updates its tables when it starts. See
[decision 8](docs/decisions/0008-users-in-postgres.md).

Personal access tokens (`rgpat_…`) work in place of a database user's
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

A production server on several Linux computers, behind a load balancer
that terminates HTTPS:

```toml
[server]
listen = "0.0.0.0:8080"
id = "git-1"                       # this server in [routing]
cache_dir = "/var/cache/worktree"  # local SSD; can be removed at any time
tls_terminated_upstream = true     # or tls_certificate / tls_private_key
drain_delay_secs = 5               # on stop: health check fails first
shutdown_timeout_secs = 180        # on stop: wait for running requests

[storage]
prefix = "prod"

[storage.s3]
endpoint = "https://s3.example.com"
region = "eu-central-1"
bucket = "git-repositories"
access_key_id = "..."
secret_access_key = "..."          # restrict to the prefix
# ca_certificate = "/etc/worktree/storage-ca.pem"

[catalog]
kind = "storage"                   # repository names in the storage catalog
create_on_push = "write"           # a push to a missing name creates it

[database]                         # users managed with worktree user / token
url = "postgres://worktree:...@db.example.com/worktree?sslmode=verify-full"
# max_connections = 8
# timeout_secs = 5

[bus]                              # change events between servers (NATS)
url = "nats://nats.internal:4222"
# subject_prefix = "worktree"      # events go to <prefix>.repo.<id>.changed
# health_grace_secs = 5            # health check fails after this long disconnected

# [identity]                       # instead of [database]: an external service
# url = "https://id.example.com/git/check"
# token = "..."

[limits]
max_receive_bytes = 2147483648
max_object_bytes = 1073741824
max_expanded_bytes = 8589934592
max_git_processes = 64
git_cpu_secs = 900
git_memory_bytes = 4294967296      # Linux
min_free_disk_bytes = 10737418240

[[customers]]
name = "acme"
max_git_processes = 16
max_requests_per_minute = 1200
max_receive_bytes_per_hour = 21474836480

[routing]
servers = [
  { id = "git-1", url = "http://10.0.0.1:8080" },
  { id = "git-2", url = "http://10.0.0.2:8080" },
  { id = "git-3", url = "http://10.0.0.3:8080" },
]
forward = true                     # send requests to the preferred server

[maintenance]
snapshot_after_entries = 200       # the default; on the preferred server only
```

With `catalog.kind = "config"` (the default), repositories are `[[repos]]`
tables in this file. Access levels are `none`, `read`, `write`, and `force`
(also non-fast-forward updates and tag moves). Repositories can have ref
rules:

```toml
[[repos]]
name = "org/app"
id = "0d6c5b6e-2a0f-4f5e-9d49-3f1f4e7a1c55"
customer = "acme"

[[repos.ref_rules]]
pattern = "refs/heads/main"
delete = "force"

[[repos.ref_rules]]
pattern = "refs/heads/release/*"
update = "force"
```

With `catalog.create_on_push`, `git push` to a missing name creates the
repository when the user has that access (at least `write`) to the name;
the first push also sets the default branch if it does not contain `main`.
See [decision 5](docs/decisions/0005-create-on-push.md).

Plain `password` credentials work only on the loopback address. The
configuration file reloads when it changes; each reload increases the
permission-rule version that log entries record.

## Operation

- `GET /_worktree/health`: the load balancer check. 503 while the storage
  service does not answer (checked at most once per second), while the NATS
  connection of `[bus]` is down for longer than `health_grace_secs` or has
  not been made yet, or while the server stops. `GET /_worktree/live` only shows that the process runs.
- On `SIGTERM` or `SIGINT` the server stops cleanly: the health check fails
  for `drain_delay_secs` while requests are still served, then the server
  accepts no new connections and waits up to `shutdown_timeout_secs` for
  running requests and pushes. Responses meanwhile carry
  `x-worktree-draining: 1`, and other servers stop forwarding to it. A
  second signal stops at once, which is also safe. Allow the service
  manager enough time (for systemd, `TimeoutStopSec` above the sum).
- `GET /_worktree/metrics`: counters and median, 95th, and 99th percentile
  delays per push step, in Prometheus text format. It needs no sign-in;
  keep it away from the public network.
- After an accepted push, the server publishes the repository and generation
  on NATS; other servers then update their caches. Lost events are harmless:
  every request checks the storage head. See
  [decision 12](docs/decisions/0012-nats-change-bus.md).
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

The image holds only worktree, the pinned Git, tini as PID 1, and CA
certificates. It runs `worktree-server` as user 10001 and reads
`/etc/worktree/worktree.toml`; mount the configuration as a secret, and the
cache at `/var/cache/worktree` on local disk. Set the container stop timeout
above `drain_delay_secs` plus `shutdown_timeout_secs`, and keep
`git_memory_bytes` times `max_git_processes` below the memory limit.

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

The Cargo workspace has three crates:

| Crate | Contents |
|---|---|
| `crates/core` (`worktree-core`) | Configuration, record formats, storage, catalog, identities, repository creation and deletion, history reading, data removal, and backup. |
| `crates/server` (`worktree-server`) | The Git process wrapper, the repository engine (caches and pushes), snapshots, Git HTTP requests, access checks, limits, routing, change events, server assembly, and the `proc-receive` hook program. |
| `crates/cli` (`worktree-cli`, binary `worktree`) | The administration commands. It does not depend on the server crate. |

## License

[Functional Source License 1.1, Apache 2.0 Future License](LICENSE.md)
(`FSL-1.1-ALv2`). You may use, change, and redistribute the code for any
purpose other than a product or service that competes with it. Each version
becomes available under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
two years after it is released.
