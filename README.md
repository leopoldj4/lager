# rustgit

A Git server in Rust that keeps repositories in an S3-compatible storage
service. Clients use standard Git over smart HTTP. Any number of servers
share one storage service; each has a local cache that it can rebuild at
any time. A head object changed only with compare-and-swap orders all
changes of a repository.

- [Design](ARCHITECTURE.md) and [implementation plan](IMPLEMENTATION_PLAN.md)
- [Decisions](docs/decisions/) made during implementation

## Local start

One server, a local S3-compatible service (SeaweedFS), PostgreSQL for
managed users, and development credentials. Storage data, the database,
and caches live in separate directories under `./.local`; stored data is
kept between runs.

```bash
nix run .#local
```

```bash
git clone http://dev:dev@127.0.0.1:8080/demo.git
```

Without Nix, `dev/start-local.sh` uses `weed`, `initdb`, `pg_ctl`,
`createdb`, `git`, and `rustgit` from `PATH`. Git 2.45 or later is required.

## Commands

```text
rustgit [-c rustgit.toml] serve
rustgit login <url> [--user <name>]          # asks for what the URL lacks
rustgit logout
rustgit token create <name> [--access repo=level]... [--expires-days N]
rustgit token list
rustgit token delete <id>
rustgit repo create <name> [--default-branch main]
rustgit repo delete <name>
rustgit repo set-default-branch <name> <branch>
rustgit repo list
rustgit snapshot <name>                      # snapshot and combined pack
rustgit cleanup <name> [--delete]            # report, or remove, obsolete data
rustgit backup export <name> <dir>
rustgit backup import <dir> [--name <name>]
rustgit hash-password < password.txt         # for password_hash
```

After `rustgit login https://git.example.com`, the `repo` commands go
through that server's admin API with your rustgit user name and password
(which needs `"*" = "admin"`), so they need no configuration file or storage
keys. `login` checks the credentials first, creates a personal access token,
and keeps that in `~/.config/rustgit/login.toml`, readable only by you;
`logout` deletes the token. Users in the server's configuration file have no
tokens, so their password is kept instead. With `-c` (or
`RUSTGIT_CONFIG`), commands use the storage service directly, as before.
See [decision 6](docs/decisions/0006-command-line-login.md).

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
cache_dir = "/var/cache/rustgit"   # local SSD; can be removed at any time
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
# ca_certificate = "/etc/rustgit/storage-ca.pem"

[catalog]
kind = "storage"                   # repository names in the storage catalog
create_on_push = "write"           # a push to a missing name creates it

[database]                         # users managed through the admin API
url = "postgres://rustgit:...@db.example.com/rustgit?sslmode=verify-full"
# max_connections = 8
# timeout_secs = 5

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
notify = true                      # tell other servers about pushes
peer_token = "..."

[maintenance]
snapshot_after_entries = 200       # the default; on the preferred server only
```

With `catalog.kind = "config"` (the default), repositories are `[[repos]]`
tables in this file. Access levels are `read`, `write`, `force` (also
non-fast-forward updates and tag moves), and `admin` (also maintenance).
Repositories can have ref rules:

```toml
[[repos]]
name = "org/app"
id = "0d6c5b6e-2a0f-4f5e-9d49-3f1f4e7a1c55"
customer = "acme"

[[repos.ref_rules]]
pattern = "refs/heads/main"
force = "admin"
delete = "admin"

[[repos.ref_rules]]
pattern = "refs/heads/release/*"
update = "admin"
```

With `catalog.create_on_push`, `git push` to a missing name creates the
repository when the user has that access (at least `write`) to the name;
the first push also sets the default branch if it does not contain `main`.
See [decision 5](docs/decisions/0005-create-on-push.md).

Plain `password` credentials work only on the loopback address. The
configuration file reloads when it changes; each reload increases the
permission-rule version that log entries record.

## Operation

- `GET /_rustgit/health`: the load balancer check. 503 while the storage
  service does not answer (checked at most once per second) or while the
  server stops. `GET /_rustgit/live` only shows that the process runs.
- On `SIGTERM` or `SIGINT` the server stops cleanly: the health check fails
  for `drain_delay_secs` while requests are still served, then the server
  accepts no new connections and waits up to `shutdown_timeout_secs` for
  running requests and pushes. Responses meanwhile carry
  `x-rustgit-draining: 1`, and other servers stop forwarding to it. A
  second signal stops at once, which is also safe. Allow the service
  manager enough time (for systemd, `TimeoutStopSec` above the sum).
- `GET /_rustgit/metrics`: counters and median, 95th, and 99th percentile
  delays per push step, in Prometheus text format.
- `POST /_rustgit/snapshot?repo=<name>` and `POST /_rustgit/rebuild?repo=<name>`
  (admin access): create a snapshot, or rebuild the local cache from storage.
- `/_rustgit/api/`: JSON for user management and the repository list, for
  users with `"*" = "admin"`. With `[database]`, users created there are
  kept in PostgreSQL and need no configuration change; users in the file
  take precedence, so keep one admin there. The server creates and updates
  its tables when it starts. See [decision 8](docs/decisions/0008-users-in-postgres.md).
- Personal access tokens (`rgpat_…`) work in place of the password for Git
  and the API, for database users: `git clone https://alice:rgpat_…@git.example.com/org/app.git`.
  `rustgit token create` makes one, optionally limited to some repositories
  (`--access org/app=read`) and with an expiry. A token never has more access
  than its user and cannot create other tokens. See
  [decision 9](docs/decisions/0009-personal-access-tokens.md).
- Every response has an `x-rustgit-request-id` header. A push uses the same
  UUID as its request UUID in the log entry and in the client's
  `remote: rustgit: accepted ...` line. Logs never contain credentials or
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

The image holds only rustgit, the pinned Git, tini as PID 1, and CA
certificates. It runs as user 10001 and reads
`/etc/rustgit/rustgit.toml`; mount the configuration as a secret, and the
cache at `/var/cache/rustgit` on local disk. Set the container stop timeout
above `drain_delay_secs` plus `shutdown_timeout_secs`, and keep
`git_memory_bytes` times `max_git_processes` below the memory limit.

On a Mac, an OrbStack NixOS machine works as the Linux builder: it sees
the Mac's files, so this builds and loads the image into OrbStack's Docker:

```bash
orb create nixos nix-builder
orb -m nix-builder nix --extra-experimental-features "nix-command flakes" build "$PWD#docker" --no-link --print-out-paths | xargs -I{} orb -m nix-builder {} | docker load
```

That builds for the Mac's architecture (arm64). For x86_64 servers, name
the system: the same machine builds it with Rosetta. Only rustgit is compiled; everything else comes from the binary
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
