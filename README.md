# rustgit

A Git server in Rust that keeps repositories in an S3-compatible storage
service. Clients use standard Git over smart HTTP. Any number of servers
share one storage service; each has a local cache that it can rebuild at
any time. A head object changed only with compare-and-swap orders all
changes of a repository.

- [implementation plan](IMPLEMENTATION_PLAN.md)
- [Decisions](docs/decisions/) made during implementation
- [Tests and plan checks](docs/TESTING.md)

## Local start

One server, a local S3-compatible service (SeaweedFS), and development
credentials. Storage data and caches live in separate directories under
`./.local`; stored data is kept between runs.

```bash
nix run .#local
```

```bash
git clone http://dev:dev@127.0.0.1:8080/demo.git
```

Without Nix, `dev/start-local.sh` uses `weed`, `git`, and `rustgit` from
`PATH`. Git 2.45 or later is required.

## Commands

```text
rustgit [-c rustgit.toml] serve
rustgit repo create <name> [--default-branch main]
rustgit repo delete <name>
rustgit repo set-default-branch <name> <branch>
rustgit repo list
rustgit snapshot <name>                      # snapshot and combined pack
rustgit cleanup <name> [--delete]            # report, or remove, obsolete data
rustgit backup export <name> <dir>
rustgit backup import <dir> [--name <name>]
rustgit check-storage                        # test the storage requirements
rustgit hash-password < password.txt         # for password_hash
```

`cleanup` reports only, unless `--delete` is given. Run the report first
and review it. Packs and records have separate retention periods
(`--pack-retention-hours`, `--record-retention-hours`); see
[decision 2](docs/decisions/0002-data-removal-fence.md).

Run `check-storage` against each provider before use and before each
release: S3 compatibility alone does not guarantee conditional writes.

## Configuration

A production server on several Linux computers, behind a load balancer
that terminates HTTPS:

```toml
[server]
listen = "0.0.0.0:8080"
id = "git-1"                       # this server in [routing]
cache_dir = "/var/cache/rustgit"   # local SSD; can be removed at any time
tls_terminated_upstream = true     # or tls_certificate / tls_private_key

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

[identity]                         # or [[users]] with password_hash
url = "https://id.example.com/git/check"
token = "..."

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
snapshot_after_entries = 200       # on the preferred server only
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

Plain `password` credentials work only on the loopback address. The
configuration file reloads when it changes; each reload increases the
permission-rule version that log entries record.

## Operation

- `GET /_rustgit/metrics`: counters and median, 95th, and 99th percentile
  delays per push step, in Prometheus text format.
- `POST /_rustgit/snapshot?repo=<name>` and `POST /_rustgit/rebuild?repo=<name>`
  (admin access): create a snapshot, or rebuild the local cache from storage.
- Every response has an `x-rustgit-request-id` header. A push uses the same
  UUID as its request UUID in the log entry and in the client's
  `remote: rustgit: accepted ...` line. Logs never contain credentials or
  repository contents.
- If the storage service is unavailable, new reads and writes fail with 503.
  A push whose result is unknown says so; the client should fetch before it
  pushes again.

## Development

```bash
nix develop
cargo test
cargo test --test provider_s3 -- --ignored    # needs SeaweedFS (dev shell)
cargo test --release --test load -- --ignored --nocapture
nix flake check
```
