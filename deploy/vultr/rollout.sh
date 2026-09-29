#!/usr/bin/env bash
# Deploys an image tag, and the current configuration, to every server,
# one at a time. Each restart drains the server first (SIGTERM): the load
# balancer stops sending it requests while running pushes finish.
#
#   ./rollout.sh <image tag>          # rustgit
#   ./rollout.sh --web <image tag>    # the management interface (web/)
set -euo pipefail
cd "$(dirname "$0")"
web=false
if [[ ${1:-} == --web ]]; then
  web=true
  shift
fi
tag=${1:?usage: rollout.sh [--web] <image tag>}

out() { ./tf output -raw "$1"; }
registry=$(out registry)
host=${registry%%/*}
reg_user=$(out registry_username)
reg_pass=$(out registry_password)
ips=$(./tf output -json server_ips | python3 -c 'import json,sys; print(" ".join(json.load(sys.stdin)))')

ssh_opts=(-i ~/.ssh/rustgit_ed25519 -o StrictHostKeyChecking=accept-new
  -o UserKnownHostsFile="$PWD/.secrets/known_hosts" -o BatchMode=yes)
mkdir -p .secrets
run() { ssh "${ssh_opts[@]}" "root@$1" "${@:2}"; }

# Host settings that cloud-init does not make, repeated on every rollout.
# The Debian image enables ufw (incoming: deny): the management interface
# reaches rustgit through the Docker bridge, which counts as incoming. SSH
# is open to every address (admin_cidrs), so only keys are accepted.
prepare() {
  run "$1" 'ufw allow in on docker0 to any port 8080 proto tcp \
      comment "rustgit-web to rustgit" >/dev/null &&
    printf "PasswordAuthentication no\nKbdInteractiveAuthentication no\n" \
      > /etc/ssh/sshd_config.d/00-rustgit.conf &&
    sshd -t && systemctl reload ssh'
}

# Waits until a command succeeds on the server, or stops the rollout.
healthy() {
  local ip=$1 check=$2 unit=$3
  for _ in $(seq 60); do
    if run "$ip" "$check" >/dev/null 2>&1; then
      echo "   healthy"
      return
    fi
    sleep 3
  done
  echo "   $ip did not become healthy; stopping the rollout" >&2
  run "$ip" "journalctl -u $unit -n 30 --no-pager" >&2 || true
  exit 1
}

if $web; then
  image="$registry-web:$tag"
  # The same secret on every server: the load balancer alternates between
  # them, and each must read the session cookie of the other.
  secret_file=.secrets/web-session-secret
  [[ -s $secret_file ]] || (umask 077 && openssl rand -hex 32 >"$secret_file")
  env="RUSTGIT_URL=http://host.docker.internal:8080
RUSTGIT_PUBLIC_URL=$(out git_url)
SESSION_SECRET=$(<"$secret_file")
NODE_OPTIONS=--max-old-space-size=192"
  # Not in cloud-init: servers created before the interface get it here.
  unit='[Unit]
Description=rustgit management interface
Requires=docker.service
After=docker.service network-online.target
Wants=network-online.target
ConditionPathExists=/etc/rustgit-web/web.env
ConditionPathExists=/etc/rustgit-web/image.env

[Service]
EnvironmentFile=/etc/rustgit-web/image.env
ExecStartPre=-/usr/bin/docker rm -f rustgit-web
ExecStartPre=/usr/bin/docker pull -q ${IMAGE}
# rustgit on this server answers the admin API calls.
ExecStart=/usr/bin/docker run --rm --name rustgit-web \
  --memory 256m --add-host=host.docker.internal:host-gateway \
  -p 3000:3000 --env-file /etc/rustgit-web/web.env ${IMAGE}
ExecStop=/usr/bin/docker stop -t 10 rustgit-web
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target'
  for ip in $ips; do
    echo "== $ip: $image"
    run "$ip" 'cloud-init status --wait >/dev/null'
    prepare "$ip"
    run "$ip" 'install -d -m 0700 /etc/rustgit-web && umask 077 &&
      cat > /etc/rustgit-web/web.env.new &&
      mv /etc/rustgit-web/web.env.new /etc/rustgit-web/web.env' <<<"$env"
    run "$ip" 'cat > /etc/systemd/system/rustgit-web.service &&
      systemctl daemon-reload && systemctl enable -q rustgit-web' <<<"$unit"
    run "$ip" "docker login -u '$reg_user' --password-stdin '$host' >/dev/null" <<<"$reg_pass"
    run "$ip" "docker pull -q '$image' >/dev/null &&
      echo 'IMAGE=$image' > /etc/rustgit-web/image.env &&
      systemctl restart rustgit-web"
    healthy "$ip" 'curl -sf -o /dev/null http://127.0.0.1:3000/login' rustgit-web
  done
  exit 0
fi

image="$registry:$tag"
config=$(out rustgit_config)
for ip in $ips; do
  echo "== $ip: $image"
  # Wait for cloud-init on new servers.
  run "$ip" 'cloud-init status --wait >/dev/null'
  prepare "$ip"
  run "$ip" 'umask 077; cat > /etc/rustgit/rustgit.toml.new &&
    chown 10001:10001 /etc/rustgit/rustgit.toml.new &&
    chmod 0400 /etc/rustgit/rustgit.toml.new &&
    mv /etc/rustgit/rustgit.toml.new /etc/rustgit/rustgit.toml' <<<"$config"
  run "$ip" "docker login -u '$reg_user' --password-stdin '$host' >/dev/null" <<<"$reg_pass"
  run "$ip" "docker pull -q '$image' >/dev/null &&
    echo 'IMAGE=$image' > /etc/rustgit/image.env &&
    systemctl restart rustgit"
  healthy "$ip" 'curl -sf http://127.0.0.1:8080/_rustgit/health' rustgit
done
