#!/usr/bin/env bash
# Local mode (IMPLEMENTATION_PLAN.md, Stage 1): one rustgit server and one
# local S3-compatible service (SeaweedFS), with development credentials.
#
#   nix run .#local            # pinned SeaweedFS, Git, and rustgit
#   dev/start-local.sh         # uses weed, git, and rustgit from PATH
#
# Storage data and caches use separate directories under $RUSTGIT_LOCAL_DIR
# (default: ./.local). Stored data is kept between runs. Remove the cache
# directory at any time: the server recovers from storage.
set -euo pipefail

root="${RUSTGIT_LOCAL_DIR:-$PWD/.local}"
storage_dir="$root/storage"
cache_dir="$root/cache"
listen="${RUSTGIT_LISTEN:-127.0.0.1:8080}"
s3_port="${RUSTGIT_S3_PORT:-8333}"
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

[[users]]
name = "dev"
password = "dev"
access = { "*" = "admin" }
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
  wait 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# The storage requirements must hold before the server starts.
for _ in $(seq 60); do
  if "$rustgit" -c "$config" check-storage >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$weed_pid" 2>/dev/null; then
    echo "SeaweedFS stopped; see $root/storage.log" >&2
    exit 1
  fi
  sleep 1
done
"$rustgit" -c "$config" check-storage

if ! "$rustgit" -c "$config" repo list | grep -q '^demo'; then
  "$rustgit" -c "$config" repo create demo
fi

echo "clone with: git clone http://dev:dev@$listen/demo.git"
"$rustgit" -c "$config" serve &
server_pid=$!
wait "$server_pid"
