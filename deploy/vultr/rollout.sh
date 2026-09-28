#!/usr/bin/env bash
# Deploys an image tag, and the current configuration, to every server,
# one at a time. Each restart drains the server first (SIGTERM): the load
# balancer stops sending it requests while running pushes finish.
#
#   ./rollout.sh <image tag>
set -euo pipefail
cd "$(dirname "$0")"
tag=${1:?usage: rollout.sh <image tag>}

out() { ./tf output -raw "$1"; }
registry=$(out registry)
image="$registry:$tag"
host=${registry%%/*}
config=$(out rustgit_config)
reg_user=$(out registry_username)
reg_pass=$(out registry_password)
ips=$(./tf output -json server_ips | python3 -c 'import json,sys; print(" ".join(json.load(sys.stdin)))')

ssh_opts=(-i ~/.ssh/rustgit_ed25519 -o StrictHostKeyChecking=accept-new
  -o UserKnownHostsFile="$PWD/.secrets/known_hosts" -o BatchMode=yes)
mkdir -p .secrets
run() { ssh "${ssh_opts[@]}" "root@$1" "${@:2}"; }

for ip in $ips; do
  echo "== $ip: $image"
  # Wait for cloud-init on new servers.
  run "$ip" 'cloud-init status --wait >/dev/null'
  run "$ip" 'umask 077; cat > /etc/rustgit/rustgit.toml.new &&
    chown 10001:10001 /etc/rustgit/rustgit.toml.new &&
    chmod 0400 /etc/rustgit/rustgit.toml.new &&
    mv /etc/rustgit/rustgit.toml.new /etc/rustgit/rustgit.toml' <<<"$config"
  run "$ip" "docker login -u '$reg_user' --password-stdin '$host' >/dev/null" <<<"$reg_pass"
  run "$ip" "docker pull -q '$image' >/dev/null &&
    echo 'IMAGE=$image' > /etc/rustgit/image.env &&
    systemctl restart rustgit"
  for _ in $(seq 60); do
    if run "$ip" 'curl -sf http://127.0.0.1:8080/_rustgit/health >/dev/null'; then
      echo "   healthy"
      continue 2
    fi
    sleep 3
  done
  echo "   $ip did not become healthy; stopping the rollout" >&2
  run "$ip" 'journalctl -u rustgit -n 30 --no-pager' >&2 || true
  exit 1
done
