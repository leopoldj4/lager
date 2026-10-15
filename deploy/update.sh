#!/usr/bin/env bash
# Runs on the Compose host, after building or loading the image. Also usable over SSH stdin.
set -Eeuo pipefail

image=${1:?usage: update.sh worktree:<full-commit-sha> [compose-directory]}
[[ "$image" =~ ^worktree:[0-9a-f]{40}$ ]] || {
  echo 'Expected an image tagged with the full Git commit SHA.' >&2
  exit 1
}
cd "${2:-/opt/worktree}"

# Serialize CI and manual invocations on this host as well as in Actions.
exec 9>.deploy.lock
flock -w 600 9

docker image inspect "$image" >/dev/null
test -f .env
# Validate configuration before changing the running service.
docker compose config --quiet
cp -p .env .env.before-update

healthy() {
  local container ip
  for ((attempt = 0; attempt < 30; attempt++)); do
    container=$(docker compose ps -q worktree) || container=
    ip=
    if [[ -n "$container" ]]; then
      ip=$(docker inspect --format '{{range .NetworkSettings.Networks}}{{println .IPAddress}}{{end}}' "$container" | head -n 1) || ip=
    fi
    if [[ -n "$ip" ]] && curl --fail --silent --show-error --connect-timeout 2 --max-time 3 \
        "http://$ip:8080/_worktree/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 2
  done
  return 1
}

rollback() {
  local status=$?
  trap - EXIT HUP INT TERM
  if ((status != 0)); then
    echo 'Deployment failed; restoring the previous image.' >&2
    docker compose logs --tail 50 worktree >&2 || true
    cp -p .env.before-update .env
    if docker compose up -d --no-deps --pull never worktree && healthy; then
      echo 'Previous image restored and healthy.' >&2
    else
      echo 'ROLLBACK FAILED: inspect /opt/worktree and the worktree container.' >&2
    fi
  fi
  exit "$status"
}
trap rollback EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

# Preserve every other setting and the file permissions. Do not source .env:
# Compose env files are not shell scripts.
python3 - "$image" <<'PY'
from pathlib import Path
import re
import sys

path = Path('.env')
lines = path.read_text().splitlines(keepends=True)
pattern = re.compile(r'^\s*(?:export\s+)?WORKTREE_IMAGE\s*=')
lines = [line for line in lines if not pattern.match(line)]
text = ''.join(lines)
if text and not text.endswith('\n'):
    text += '\n'
path.write_text(text + f'WORKTREE_IMAGE={sys.argv[1]}\n')
PY

docker compose up -d --no-deps --pull never worktree
healthy
trap - EXIT HUP INT TERM
echo "Deployed $image; health check passed."
