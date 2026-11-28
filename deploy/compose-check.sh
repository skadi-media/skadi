#!/usr/bin/env bash
# Render every compose combination with placeholder env (SKADI-T-0485).
#
# Nothing validated the compose files before this: a broken overlay key or a
# typo'd interpolation surfaced at deploy time, on the machine being deployed to.
#
# Renders base, base+lab, and both with the secrets overlay. Runs from a scratch copy of `deploy/`
# because it writes a placeholder `.env`, and on a machine running the stack the
# real `deploy/.env` holds live credentials that must not be clobbered.
#
# `docker compose config` only *warns* about an undefined variable, so a zero
# exit is not on its own proof the file is sound — the warnings are treated as
# failures here, which is the whole point of the check.
set -euo pipefail

repo_deploy="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cp "$repo_deploy"/docker-compose*.yml "$work/"
cp "$repo_deploy"/.env.example "$repo_deploy"/.env.lab "$work/"

fail=0

render() {
  local name="$1" envs="$2"
  shift 2
  # Later definitions win, which is how the overlays' env files are meant to
  # stack.
  local paths=""
  for e in $envs; do paths="$paths $work/$e"; done
  # shellcheck disable=SC2086
  cat $paths > "$work/.env"

  local out
  if ! out="$(cd "$work" && docker compose "$@" config 2>&1 >/dev/null)"; then
    echo "FAIL  $name — compose config exited non-zero"
    echo "$out" | sed 's/^/      /'
    fail=1
    return
  fi
  if [ -n "$out" ]; then
    echo "FAIL  $name — compose warned (undefined variable or deprecated key)"
    echo "$out" | sed 's/^/      /'
    fail=1
    return
  fi
  echo "ok    $name"
}

render "base"      ".env.example"                     -f docker-compose.yml
# With a download buffer (SKADI-T-0785): a placeholder host folder.
printf 'SKADI_BUFFER_SOURCE=/srv/skadi-buffer\n' > "$work/.env.buffer"
render "base+buffer" ".env.example .env.buffer"       -f docker-compose.yml
render "base+lab"  ".env.example .env.lab"            -f docker-compose.yml -f docker-compose.lab.yml
# Secrets-from-files overlay (SKADI-T-0702). `config` does not need the secret
# files to exist, so no placeholders are written.
render "base+secrets"     ".env.example"              -f docker-compose.yml -f docker-compose.secrets.yml
render "base+secrets+lab" ".env.example .env.lab"     -f docker-compose.yml -f docker-compose.secrets.yml -f docker-compose.lab.yml

# The buffer setting follows the source: empty without one, /buffer with one;
# the worker gets the mount either way (SKADI-T-0785).
worker_config() {
  # shellcheck disable=SC2086
  cat $1 > "$work/.env"
  (cd "$work" && docker compose -f docker-compose.yml config skadi-downloader-worker 2>/dev/null)
}
off="$(worker_config "$work/.env.example")"
on="$(worker_config "$work/.env.example $work/.env.buffer")"
if echo "$off" | grep -Eq 'SKADI_WORKER_BUFFER_DIR: ("")?$' && echo "$off" | grep -q 'target: /buffer'; then
  echo "ok    buffer off: SKADI_WORKER_BUFFER_DIR empty, /buffer mounted unused"
else
  echo "FAIL  buffer off: expected an empty SKADI_WORKER_BUFFER_DIR"; fail=1
fi
if echo "$on" | grep -q 'SKADI_WORKER_BUFFER_DIR: /buffer$' && echo "$on" | grep -q 'source: /srv/skadi-buffer'; then
  echo "ok    buffer on: SKADI_WORKER_BUFFER_DIR=/buffer from /srv/skadi-buffer"
else
  echo "FAIL  buffer on: expected SKADI_WORKER_BUFFER_DIR=/buffer and source /srv/skadi-buffer"; fail=1
fi

exit "$fail"
