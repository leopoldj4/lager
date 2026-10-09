#!/usr/bin/env bash
# Local mode (IMPLEMENTATION_PLAN.md, Stage 1): one rustgit server, one
# local S3-compatible service (SeaweedFS), and PostgreSQL for managed users,
# with development credentials.
#
#   nix run .#local            # pinned SeaweedFS, PostgreSQL, Git, and rustgit
#   dev/start-local.sh         # uses weed, initdb, pg_ctl, git, and rustgit from PATH
#
# Storage data, database, and caches use separate directories under
# $RUSTGIT_LOCAL_DIR (default: ./.local). Stored data is kept between runs.
# Remove the cache directory at any time: the server recovers from storage.
set -euo pipefail

root="${RUSTGIT_LOCAL_DIR:-$PWD/.local}"
storage_dir="$root/storage"
cache_dir="$root/cache"
listen="${RUSTGIT_LISTEN:-127.0.0.1:8080}"
s3_port="${RUSTGIT_S3_PORT:-8333}"
pg_port="${RUSTGIT_PG_PORT:-5433}"
pg_dir="$root/postgres"
weed="${WEED:-weed}"
rustgit="${RUSTGIT:-rustgit}"
mkdir -p "$storage_dir" "$cache_dir"

s3_config="$root/s3.json"
cat >"$s3_config" <<'EOF'
{"identities": [{"name": "rustgit-dev",
  "credentials": [{"accessKey": "rustgit-dev", "secretKey": "rustgit-dev-secret"}],
  "actions": ["Admin", "Read", "Write", "List", "Tagging"]}]}
EOF

config="$root/rustgit.toml"
if [[ ! -f "$config" ]]; then
  cat >"$config" <<EOF
# Local development configuration. Plain passwords work only on the
# loopback address; use password_hash (rustgit hash-password) elsewhere.
[server]
listen = "$listen"
cache_dir = "$cache_dir"
drain_delay_secs = 0

[storage]
prefix = "local"

[storage.s3]
endpoint = "http://127.0.0.1:$s3_port"
bucket = "rustgit"
access_key_id = "rustgit-dev"
secret_access_key = "rustgit-dev-secret"
allow_http = true

[catalog]
kind = "storage"                  # repositories and managed users in storage
cache_secs = 1
create_on_push = "write"          # git push to a new name creates the repository

[database]                        # users for rustgit user / rustgit token
url = "postgres://rustgit@127.0.0.1:$pg_port/rustgit"

[[users]]
name = "dev"
password = "dev"
access = { "*" = "force" }
EOF
fi

"$weed" mini -dir="$storage_dir" -ip=127.0.0.1 -ip.bind=127.0.0.1 \
  -s3.port="$s3_port" -master.port="${RUSTGIT_WEED_MASTER_PORT:-9333}" \
  -volume.port="${RUSTGIT_WEED_VOLUME_PORT:-8081}" \
  -filer.port="${RUSTGIT_WEED_FILER_PORT:-8888}" \
  -admin.ui=false -webdav=false -s3.config="$s3_config" -bucket=rustgit \
  >"$root/storage.log" 2>&1 &
weed_pid=$!
server_pid=""
cleanup() {
  kill ${server_pid:+"$server_pid"} "$weed_pid" 2>/dev/null || true
  pg_ctl -D "$pg_dir" -m fast stop >/dev/null 2>&1 || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# PostgreSQL on the loopback address only, without a password, and without
# a Unix socket. rustgit creates its tables when it starts.
if [[ ! -f "$pg_dir/PG_VERSION" ]]; then
  initdb -D "$pg_dir" -U rustgit --auth=trust >"$root/postgres-init.log"
fi
pg_ctl -D "$pg_dir" -l "$root/postgres.log" -w \
  -o "-h 127.0.0.1 -p $pg_port -k ''" start >/dev/null
createdb -h 127.0.0.1 -p "$pg_port" -U rustgit rustgit 2>/dev/null || true

# Wait until the storage service answers.
for _ in $(seq 60); do
  if repos=$("$rustgit" -c "$config" repo list 2>/dev/null); then
    break
  fi
  if ! kill -0 "$weed_pid" 2>/dev/null; then
    echo "SeaweedFS stopped; see $root/storage.log" >&2
    exit 1
  fi
  sleep 1
done
repos=$("$rustgit" -c "$config" repo list)

if ! grep -q '^demo' <<<"$repos"; then
  "$rustgit" -c "$config" repo create demo
fi

echo "clone with: git clone http://dev:dev@$listen/demo.git"
"$rustgit" -c "$config" serve &
server_pid=$!
wait "$server_pid"
