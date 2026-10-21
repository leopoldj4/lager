#!/usr/bin/env bash
# Local mode: SeaweedFS, PostgreSQL, and NATS in Docker
# (local.compose.yaml), and one server built from this checkout with a
# development user. The configuration (lager.env), the SSH host key, and
# the cache live under $LAGER_LOCAL_DIR (default: ./.local); remove the
# cache directory at any time: the server recovers from storage. Stored
# data is kept between runs in the Docker volumes.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
export LAGER_LOCAL_DIR="${LAGER_LOCAL_DIR:-$PWD/.local}"
root="$LAGER_LOCAL_DIR"
cache_dir="$root/cache"
listen="${LAGER_LISTEN:-127.0.0.1:8080}"
ssh_listen="${LAGER_SSH_LISTEN:-127.0.0.1:2222}"
export LAGER_S3_PORT="${LAGER_S3_PORT:-8333}"
export LAGER_PG_PORT="${LAGER_PG_PORT:-5433}"
export LAGER_NATS_PORT="${LAGER_NATS_PORT:-4222}"
mkdir -p "$cache_dir"

# The web interface, served from share/lager/web beside the programs.
(
  cd "$repo/web"
  bun install --frozen-lockfile
  bun run build
) >"$root/web-build.log" 2>&1 || { echo "web build failed; see $root/web-build.log" >&2; exit 1; }
cargo build --manifest-path "$repo/Cargo.toml" --bin lager --bin lager-server
target="${CARGO_TARGET_DIR:-$repo/target}"
mkdir -p "$target/share/lager"
ln -sfn "$repo/web/dist/client" "$target/share/lager/web"
PATH="$target/debug:$PATH"

cat >"$root/s3.json" <<'JSON'
{"identities": [{"name": "lager-dev",
  "credentials": [{"accessKey": "lager-dev", "secretKey": "lager-dev-secret"}],
  "actions": ["Admin", "Read", "Write", "List", "Tagging"]}]}
JSON

# The SSH host key; clients remember it, so keep it between runs.
host_key="$root/ssh_host_ed25519_key"
if [[ ! -f "$host_key" ]]; then
  ssh-keygen -q -t ed25519 -N '' -C lager-local -f "$host_key"
fi

# The configuration, for lager-server and lager; to run lager by hand:
# set -a; . .local/lager.env; set +a. The session secret stays between
# runs, so that sign-ins do too.
env_file="$root/lager.env"
session_secret=$(sed -n 's/^LAGER_SESSION_SECRET=//p' "$env_file" 2>/dev/null || true)
if [[ -z "$session_secret" ]]; then
  session_secret=$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')
fi
cat >"$env_file" <<ENV
LAGER_LISTEN=$listen
LAGER_CACHE_DIR=$cache_dir
LAGER_S3_ENDPOINT=http://127.0.0.1:$LAGER_S3_PORT
LAGER_S3_BUCKET=lager
LAGER_S3_ACCESS_KEY_ID=lager-dev
LAGER_S3_SECRET_ACCESS_KEY=lager-dev-secret
LAGER_S3_PREFIX=local
LAGER_DATABASE_URL=postgres://lager@127.0.0.1:$LAGER_PG_PORT/lager
LAGER_NATS_URL=nats://127.0.0.1:$LAGER_NATS_PORT
LAGER_SESSION_SECRET=$session_secret
LAGER_SSH_LISTEN=$ssh_listen
LAGER_SSH_HOST_KEY=$(base64 <"$host_key" | tr -d '\n')
ENV
set -a
# shellcheck source=/dev/null
. "$env_file"
set +a

# GitHub mirrors need an OAuth app with callback URL
# http://$listen/-/api/github/callback; set LAGER_GITHUB_CLIENT_ID and
# LAGER_GITHUB_CLIENT_SECRET before starting.

compose=(docker compose -f "$repo/scripts/local.compose.yaml")
"${compose[@]}" up -d --wait
server_pid=""
cleanup() {
  [[ -n "$server_pid" ]] && kill "$server_pid" 2>/dev/null || true
  wait 2>/dev/null || true
  "${compose[@]}" stop >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# Wait until the storage service answers.
for _ in $(seq 60); do
  if repos=$(lager repo list 2>/dev/null); then
    break
  fi
  sleep 1
done
repos=$(lager repo list)

if ! grep -q '^dev/demo' <<<"$repos"; then
  lager repo create dev/demo
fi
if ! lager user list | grep -q '^dev\b'; then
  echo dev-password | lager user create dev --access '*=force'
fi
# Your own public key, so that Git over SSH works as dev.
for pub in ~/.ssh/id_ed25519.pub ~/.ssh/id_ecdsa.pub ~/.ssh/id_rsa.pub; do
  if [[ -f "$pub" ]]; then
    lager ssh-key add dev "$pub" >/dev/null 2>&1 || true
    break
  fi
done

echo "clone with: git clone http://dev:dev-password@$listen/dev/demo.git"
echo "        or: git clone ssh://git@$ssh_listen/dev/demo.git"
echo "web interface: http://$listen/ (sign in as dev / dev-password)"
lager-server &
server_pid=$!
wait "$server_pid"
