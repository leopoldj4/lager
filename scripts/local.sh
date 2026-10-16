#!/usr/bin/env bash
# Local mode: SeaweedFS, PostgreSQL, and NATS in Docker
# (local.compose.yaml), and one server built from this checkout with a
# development user. The configuration and the cache live under
# $WORKTREE_LOCAL_DIR (default: ./.local); remove the cache directory at
# any time: the server recovers from storage. Stored data is kept between
# runs in the Docker volumes.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
export WORKTREE_LOCAL_DIR="${WORKTREE_LOCAL_DIR:-$PWD/.local}"
root="$WORKTREE_LOCAL_DIR"
cache_dir="$root/cache"
listen="${WORKTREE_LISTEN:-127.0.0.1:8080}"
ssh_listen="${WORKTREE_SSH_LISTEN:-127.0.0.1:2222}"
export WORKTREE_S3_PORT="${WORKTREE_S3_PORT:-8333}"
export WORKTREE_PG_PORT="${WORKTREE_PG_PORT:-5433}"
export WORKTREE_NATS_PORT="${WORKTREE_NATS_PORT:-4222}"
mkdir -p "$cache_dir"

# The web interface, served from share/worktree/web beside the programs.
(
  cd "$repo/web"
  bun install --frozen-lockfile
  bun run build
) >"$root/web-build.log" 2>&1 || { echo "web build failed; see $root/web-build.log" >&2; exit 1; }
cargo build --manifest-path "$repo/Cargo.toml" --bin worktree --bin worktree-server
target="${CARGO_TARGET_DIR:-$repo/target}"
mkdir -p "$target/share/worktree"
ln -sfn "$repo/web/dist/client" "$target/share/worktree/web"
PATH="$target/debug:$PATH"

cat >"$root/s3.json" <<'JSON'
{"identities": [{"name": "worktree-dev",
  "credentials": [{"accessKey": "worktree-dev", "secretKey": "worktree-dev-secret"}],
  "actions": ["Admin", "Read", "Write", "List", "Tagging"]}]}
JSON

# The SSH host key; clients remember it, so keep it between runs.
host_key="$root/ssh_host_ed25519_key"
if [[ ! -f "$host_key" ]]; then
  ssh-keygen -q -t ed25519 -N '' -C worktree-local -f "$host_key"
fi

config="$root/worktree.toml"
if [[ ! -f "$config" ]]; then
  cat >"$config" <<TOML
# Local development configuration.
[server]
listen = "$listen"
cache_dir = "$cache_dir"

[storage]
endpoint = "http://127.0.0.1:$WORKTREE_S3_PORT"
bucket = "worktree"
access_key_id = "worktree-dev"
secret_access_key = "worktree-dev-secret"
prefix = "local"

[database]                        # users for worktree user / worktree token
url = "postgres://worktree@127.0.0.1:$WORKTREE_PG_PORT/worktree"

[bus]                             # change events between servers
url = "nats://127.0.0.1:$WORKTREE_NATS_PORT"

[web]                             # web interface on http://$listen/
session_secret = "$(head -c 32 /dev/urandom | base64)"
TOML
fi
# Configurations from before Git over SSH get it added.
if ! grep -q '^\[ssh\]' "$config"; then
  cat >>"$config" <<TOML

[ssh]                             # Git over SSH; keys with worktree ssh-key
listen = "$ssh_listen"
host_key = "$host_key"
public_url = "ssh://git@$ssh_listen"
TOML
fi

# GitHub mirrors need an OAuth app with callback URL
# http://$listen/-/api/github/callback.
if [[ -n "${WORKTREE_GITHUB_CLIENT_ID:-}" ]] && ! grep -q '^\[github\]' "$config"; then
  cat >>"$config" <<TOML

[github]                          # mirrors of GitHub repositories
client_id = "$WORKTREE_GITHUB_CLIENT_ID"
client_secret = "${WORKTREE_GITHUB_CLIENT_SECRET:?set WORKTREE_GITHUB_CLIENT_SECRET too}"
TOML
fi

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
  if repos=$(worktree -c "$config" repo list 2>/dev/null); then
    break
  fi
  sleep 1
done
repos=$(worktree -c "$config" repo list)

if ! grep -q '^dev/demo' <<<"$repos"; then
  worktree -c "$config" repo create dev/demo
fi
if ! worktree -c "$config" user list | grep -q '^dev\b'; then
  echo dev-password | worktree -c "$config" user create dev --access '*=force'
fi
# Your own public key, so that Git over SSH works as dev.
for pub in ~/.ssh/id_ed25519.pub ~/.ssh/id_ecdsa.pub ~/.ssh/id_rsa.pub; do
  if [[ -f "$pub" ]]; then
    worktree -c "$config" ssh-key add dev "$pub" >/dev/null 2>&1 || true
    break
  fi
done

echo "clone with: git clone http://dev:dev-password@$listen/dev/demo.git"
echo "        or: git clone ssh://git@$ssh_listen/dev/demo.git"
echo "web interface: http://$listen/ (sign in as dev / dev-password)"
worktree-server -c "$config" &
server_pid=$!
wait "$server_pid"
