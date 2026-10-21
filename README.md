# lager

A Git server in Rust that keeps repositories in an S3-compatible storage
service. Clients use standard Git over smart HTTP. Any number of servers
share one storage service; each has a local cache that it can rebuild at
any time. A head object changed only with compare-and-swap orders all
changes of a repository.

- [implementation plan](IMPLEMENTATION_PLAN.md)
- [Decisions](docs/decisions/) made during implementation

## Local start

One server built from this checkout, a local S3-compatible service
(SeaweedFS), PostgreSQL for users, NATS for change events, a development
user, and the Vite server for the web interface, which reloads on changes. SeaweedFS, PostgreSQL, and NATS run in Docker; stored
data is kept between runs in Docker volumes. The configuration
(`.local/lager.env`), the SSH host key, and the cache live under `./.local`.

```bash
scripts/local.ts        # build, start, and serve until Ctrl-C
scripts/local.ts down   # remove this checkout's containers and data
```

It prints the addresses once the server and Vite answer. Every checkout,
such as a Git worktree, has its own containers, data, and ports, so several
run side by side. The server takes 8080 and 2222, or the next free ports,
and keeps them between runs while they are free; Vite takes 3000 or the
next free port. With the defaults:

```bash
git clone http://dev:dev-password@127.0.0.1:8080/dev/demo.git
git clone ssh://git@127.0.0.1:2222/dev/demo.git   # with your key, see below
```

The script adds your `~/.ssh/id_ed25519.pub` (or ECDSA or RSA key) to `dev`
for Git over SSH ([decision 22](docs/decisions/0022-git-over-ssh.md)).

The web interface is at http://127.0.0.1:3000/ (sign in as `dev` / `dev-password`).
To run `lager` by hand, load the configuration first:
`set -a; . .local/lager.env; set +a`.

For GitHub mirrors, register a GitHub OAuth app with the callback URL
`http://127.0.0.1:8080/-/api/github/callback` and start with
`LAGER_GITHUB_CLIENT_ID=... LAGER_GITHUB_CLIENT_SECRET=... scripts/local.ts`.

It needs Rust (rustup reads `rust-toolchain.toml`), the OpenSSL headers
(`libssl-dev`), Bun, Docker with Compose, and Git 2.45 or later.

### Demo data

`scripts/seed.ts` creates two users, `demo` and `alex` (password
`demo-password`), and repositories under `demo/` to try everything out:
a public website with branches, tags, a binary file, and six pull requests
(open, conflicting, merged, squashed, closed, merged by a push), a private
API with a long history, a large diff, and ref rules, a nested repository
with the default branch `trunk`, a private one only `demo` reads, and an
empty one. `alex` has `write` on the website and `read` on the others.

```bash
bun scripts/seed.ts                                                  # the local server
bun scripts/seed.ts --url https://git.example.com --cli "<command>"  # a deployment
```

`--cli "<command>"` names the administration command for a deployment, for
example `ssh root@dokploy docker exec -i <container> lager`. Repositories
that exist already are skipped; `--reset` deletes and seeds them again.
`--user`, `--reviewer`, and `--password` change the names and the password.

## Commands

The server and the administration command line are separate programs:

```text
lager-server
```

```text
lager user create <name> [--access repo=level]...   # asks for the password
lager user list
lager user set-password <name>
lager user set-access <name> [repo=level]...       # replaces all levels
lager user delete <name>
lager token create <user> <name> [--access repo=level]... [--expires-days N]
lager token list <user>
lager token delete <user> <id>
lager ssh-key add <user> <file|-> [--name <name>]  # e.g. ~/.ssh/id_ed25519.pub
lager ssh-key list <user>
lager ssh-key delete <user> <id>
lager repo create <name> [--default-branch main]
lager repo delete <name>
lager repo set-default-branch <name> <branch>
lager repo set-public <name> true|false   # read without signing in
lager repo set-ref-rule <name> <pattern> [--update L] [--force L] [--delete L]
lager repo delete-ref-rule <name> <pattern>
lager repo show <name>
lager repo list
lager cleanup <name> [--delete]            # report, or remove, obsolete data
lager backup export <name> <dir>
lager backup import <dir> [--name <name>]
```

Both programs read the configuration from the environment (see below).
`repo`, `cleanup`, and `backup` use the
storage service directly; `user`, `token`, and `ssh-key` use the database.
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
lager repo set-ref-rule org/app refs/heads/main --delete force
lager repo set-ref-rule org/app 'refs/heads/release/*' --update force
```

Personal access tokens (`rgpat_…`) work in place of a user's
password: `git clone https://alice:rgpat_…@git.example.com/org/app.git`.
`lager token create` prints one once, optionally limited to some
repositories (`--access org/app=read`) and with an expiry. A token never has
more access than its user. See
[decision 9](docs/decisions/0009-personal-access-tokens.md).

`cleanup` reports only, unless `--delete` is given. Run the report first
and review it. Packs and records have separate retention periods
(`--pack-retention-hours`, `--record-retention-hours`); see
[decision 2](docs/decisions/0002-data-removal-fence.md).

## Configuration

Environment variables hold only what a server needs to start
([decision 24](docs/decisions/0024-configuration-in-the-environment.md)).
An empty variable counts as unset. Limits and timings are fixed in
`crates/core/src/limits.rs`.

| Variable | Default | |
|---|---|---|
| `LAGER_DOMAIN` | none | The public host name, such as `git.example.com`. Passkeys and the GitHub callback use `https://<domain>`, the clone menu `ssh://git@<domain>`. Without it, passkeys are off. `localhost` or `localhost:<port>` means `http://` for development. |
| `LAGER_LISTEN` | `0.0.0.0:8080` | Plain HTTP; a load balancer terminates HTTPS. |
| `LAGER_CACHE_DIR` | `/var/cache/lager` | Local SSD; can be removed at any time. |
| `LAGER_S3_ENDPOINT` | AWS S3 | An S3-compatible service with conditional writes; `http://` for local development. |
| `LAGER_S3_REGION` | `us-east-1` | `auto` for Cloudflare R2. |
| `LAGER_S3_BUCKET` | required | |
| `LAGER_S3_ACCESS_KEY_ID` | required | |
| `LAGER_S3_SECRET_ACCESS_KEY` | required | Restrict the key to the prefix. |
| `LAGER_S3_PREFIX` | none | Key prefix in the bucket. |
| `LAGER_DATABASE_URL` | required | PostgreSQL for users and access tokens, such as `postgres://lager:...@db.example.com/lager?sslmode=verify-full`. |
| `LAGER_NATS_URL` | none | Change events between servers. Without it, events stay on this server. |
| `LAGER_SESSION_SECRET` | none | Turns on the web interface and its API under `/-/`. At least 32 characters, the same on every server; a new one signs everyone out. |
| `LAGER_SIGN_UP` | `false` | `true`: anyone can create an account with a password, and with a passkey when `LAGER_DOMAIN` is set. |
| `LAGER_GITHUB_CLIENT_ID`, `LAGER_GITHUB_CLIENT_SECRET` | none | A GitHub OAuth app for mirrors, with the callback URL `https://<domain>/-/api/github/callback`. |
| `LAGER_SSH_HOST_KEY` | none | Turns on Git over SSH. An OpenSSH private key without a passphrase, in double quotes across its lines; the same on every server, and kept: clients refuse a changed host key. |
| `LAGER_SSH_LISTEN` | `0.0.0.0:2222` | |
| `LAGER_SSH_PORT` | `22` | The port clients connect to; the clone menu shows `ssh://git@<domain>:<port>` unless it is 22. |

Every server is plain HTTP behind a load balancer that terminates HTTPS
and spreads requests over the servers. Any server can answer any request;
it fills its cache for the repository on first use. The server that
accepts a push creates the next automatic snapshot (every 200 entries).
The web interface is served from `share/lager/web` beside the programs,
where the container image installs it; without it, only the API under
`/-/api/` answers.

## Operation

- `GET /_lager/health`: the load balancer check. 503 while the storage
  service does not answer (checked at most once per second), while the NATS
  connection of `LAGER_NATS_URL` is down for longer than 5 seconds or has
  not been made yet, or while the server stops. `GET /_lager/live` only shows that the process runs.
- On `SIGTERM` or `SIGINT` the server stops cleanly: the health check fails
  for 5 seconds while requests are still served, then the server
  accepts no new connections and waits up to 180 seconds for
  running requests and pushes. Responses meanwhile carry
  `x-lager-draining: 1`. A
  second signal stops at once, which is also safe. Allow the service
  manager enough time (for systemd, `TimeoutStopSec` above 185 seconds).
- `GET /_lager/metrics`: counters and median, 95th, and 99th percentile
  delays per push step, in Prometheus text format. It needs no sign-in;
  keep it away from the public network.
- After an accepted push, the server publishes the repository and generation
  on NATS; other servers then update their caches. Lost events are harmless:
  every request checks the storage head. See
  [decision 12](docs/decisions/0012-nats-change-bus.md).
- The web interface (`LAGER_SESSION_SECRET`) shows repositories, files, history, and
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
- GitHub mirrors (`LAGER_GITHUB_CLIENT_ID`): **Settings** in the user menu has
  **Connect with GitHub**. Then pick GitHub repositories; each becomes
  `<user>/<name>` here and follows GitHub's branches, tags, and default
  branch within about a minute. Mirrors are read-only: pushes and pull
  requests are refused. **Stop syncing** keeps the repository and makes it
  writable. See [decision 20](docs/decisions/0020-github-mirrors.md).
- Every response has an `x-lager-request-id` header. A push uses the same
  UUID as its request UUID in the log entry and in the client's
  `remote: lager: accepted ...` line. Logs never contain credentials or
  repository contents.
- If the storage service is unavailable, new reads and writes fail with 503.
  A push whose result is unknown says so; the client should fetch before it
  pushes again.

## Container image

Build it from the `Dockerfile`, tagged with the commit:

```bash
docker build -t lager:$(git rev-parse --short HEAD) .
```

The image holds lager (`/usr/local/bin/lager-server` and
`/usr/local/bin/lager`), the
built web interface (`/usr/local/share/lager/web`), Git from Debian
trixie, tini as PID 1,
and CA certificates. It runs `lager-server` as user 10001 with the
configuration from the environment; mount the cache at `/var/cache/lager`
on local disk. Set the container stop timeout
above 185 seconds, so that running pushes finish.

`deploy/dokploy.compose.yaml` runs it with NATS and PostgreSQL under
[Dokploy](https://dokploy.com), which builds the image on the server and
terminates HTTPS; repositories live in an external S3-compatible service.
`master` deploys to staging this way; see [deploy/README.md](deploy/README.md).

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
`scripts/local.ts` runs its Vite server. On its own, Vite forwards the API
to the server in `LAGER_URL` (default http://127.0.0.1:8080):

```bash
cd web && bun install
bun run dev        # http://localhost:3000, API and WebSocket go to the server
bun run build      # dist/client, what the server serves
bun run typecheck
```

The Cargo workspace has three crates:

| Crate | Contents |
|---|---|
| `crates/core` (`lager-core`) | Configuration, fixed limits, record formats, storage, catalog, identities, repository creation and deletion, history reading, data removal, and backup. |
| `crates/server` (`lager-server`) | The Git process wrapper, the repository engine (caches and pushes), snapshots, Git HTTP requests, access checks, limits, change events, the web API and live connection, server assembly, and the `proc-receive` hook program. |
| `web` | The web interface. The server serves its built files. |
| `crates/cli` (`lager-cli`, binary `lager`) | The administration commands. It does not depend on the server crate. |

## License

[Functional Source License 1.1, Apache 2.0 Future License](LICENSE.md)
(`FSL-1.1-ALv2`). You may use, change, and redistribute the code for any
purpose other than a product or service that competes with it. Each version
becomes available under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
two years after it is released.

### Passkey login

Set `LAGER_DOMAIN` (for example `git.example.com`) on every server and
restart; passkeys then use the origin `https://<domain>`. For local
development, `LAGER_DOMAIN=localhost:8080` uses `http://localhost:8080`;
for the Vite UI, use its address instead, for example `localhost:3000`, and
open that same address in the browser. The RP ID is the host name; changing
it requires enrolling new passkeys.

Sign in, open **Passkeys** in the user menu, then name
and add a passkey. On later visits, choose
**Sign in with a passkey** and select your saved account. No user name is needed. The same account page
lists and removes passkeys (up to 20 per account). Password and Git token
login remain available. Removing a passkey blocks future logins with it;
it does not revoke existing sessions. Changing the account password ends
sessions and pending ceremonies, but enrolled passkeys remain valid.

With `LAGER_SIGN_UP=true`, the sign-in page links to **Sign up**: people
pick a free user name and a password, and are signed in
([decision 25](docs/decisions/0025-password-sign-up.md)). With
`LAGER_DOMAIN` set, they can save a passkey instead; the account exists and
is signed in once the passkey is verified. Such accounts have no password:
they cannot remove their last passkey, and Git uses access tokens or SSH
keys from the settings. Either way, they own repositories under their name
and have no other access until granted. See
[decision 23](docs/decisions/0023-passkey-sign-up.md).

Schema migrations run at startup. Public credentials and five-minute,
single-use ceremony state live in PostgreSQL, so no sticky sessions are
needed. The serialization feature of
[`webauthn-rs`](https://docs.rs/webauthn-rs/0.5.5/webauthn_rs/)
is used only for server-side state, never browser storage.
