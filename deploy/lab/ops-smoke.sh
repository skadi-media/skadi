#!/bin/sh
# Ops smoke checklist for the Skadi deploy stack (SKADI-I-0057 pass P7, C37).
#
#   deploy/lab/ops-smoke.sh [-p project] [-P port] [--egress]
#
# PROPOSAL / UNEXECUTED IN THE REVIEW PASS (Docker Desktop was down). Every check
# is READ-ONLY: `docker compose ps/config`, `docker inspect`, `docker network
# inspect`, one HTTP GET. Nothing is started, stopped, built or removed.
#
# Defaults to the LAB project (skadi-lab, API on 127.0.0.1:8091). Pointing it at
# the production project `skadi` is refused unless SKADI_OPS_SMOKE_ALLOW_PROD=1
# is set explicitly — and even then it only reads.
#
# Checks (each prints PASS / FAIL / SKIP; exit status = number of FAILs):
#   1. docker daemon reachable
#   2. compose project exists and every expected service is running (not Dead/Restarting)
#   3. containers with a healthcheck are `healthy`
#   4. no container was OOM-killed on its last exit
#   5. every container has the json-file 10m×3 log cap
#   6. the project network owns its subnet and NO other network holds it (ghost-network check)
#   7. GET /api/v1/health returns {"status":"ok"} on the given port
#      (NOT /health — that is the SPA fallback and always 200)
#   8. --egress only: the worker's egress IP differs from the host's (kill-switch spot check);
#      never run this against the lab (no VPN there) — it is skipped for skadi-lab.
set -u
PROJECT=skadi-lab
PORT=8091
EGRESS=0
while [ $# -gt 0 ]; do
  case "$1" in
    -p) PROJECT=$2; shift 2 ;;
    -P) PORT=$2; shift 2 ;;
    --egress) EGRESS=1; shift ;;
    *) echo "usage: $0 [-p project] [-P port] [--egress]" >&2; exit 2 ;;
  esac
done
if [ "$PROJECT" = skadi ] && [ "${SKADI_OPS_SMOKE_ALLOW_PROD:-0}" != 1 ]; then
  echo "refusing to inspect compose project 'skadi' (production); set SKADI_OPS_SMOKE_ALLOW_PROD=1 to allow read-only checks" >&2
  exit 2
fi

fails=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; fails=$((fails + 1)); }
skip() { echo "SKIP  $*"; }

# 1. daemon
if docker info >/dev/null 2>&1; then pass "docker daemon reachable"; else fail "docker daemon not reachable (Docker Desktop down?)"; echo "$fails check(s) failed"; exit "$fails"; fi

# 2. services running
ids=$(docker ps -aq --filter "label=com.docker.compose.project=$PROJECT")
if [ -z "$ids" ]; then fail "no containers for compose project $PROJECT"; echo "$fails check(s) failed"; exit "$fails"; fi
for id in $ids; do
  name=$(docker inspect -f '{{.Name}}' "$id" | sed 's|^/||')
  state=$(docker inspect -f '{{.State.Status}}' "$id")
  case "$state" in
    running) pass "$name running" ;;
    exited)  # one-shot services exit 0 by design
      code=$(docker inspect -f '{{.State.ExitCode}}' "$id")
      if [ "$code" = 0 ]; then pass "$name exited 0 (one-shot)"; else fail "$name exited $code"; fi ;;
    *) fail "$name state=$state" ;;
  esac
done

# 3. healthchecks
for id in $ids; do
  name=$(docker inspect -f '{{.Name}}' "$id" | sed 's|^/||')
  h=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id")
  case "$h" in
    none) : ;;
    healthy) pass "$name healthy" ;;
    *) fail "$name health=$h" ;;
  esac
done

# 4. OOM
for id in $ids; do
  name=$(docker inspect -f '{{.Name}}' "$id" | sed 's|^/||')
  if [ "$(docker inspect -f '{{.State.OOMKilled}}' "$id")" = true ]; then fail "$name was OOM-killed"; fi
done
pass "no OOM-killed containers (unless listed above)"

# 5. log caps
for id in $ids; do
  name=$(docker inspect -f '{{.Name}}' "$id" | sed 's|^/||')
  drv=$(docker inspect -f '{{.HostConfig.LogConfig.Type}}' "$id")
  max=$(docker inspect -f '{{index .HostConfig.LogConfig.Config "max-size"}}' "$id")
  if [ "$drv" = json-file ] && [ -n "$max" ]; then pass "$name log cap $max"; else fail "$name has no log cap (driver=$drv max-size=$max)"; fi
done

# 6. network / ghost check
net="${PROJECT}_default"
subnet=$(docker network inspect -f '{{range .IPAM.Config}}{{.Subnet}}{{end}}' "$net" 2>/dev/null)
if [ -z "$subnet" ]; then
  fail "network $net missing"
else
  pass "network $net owns $subnet"
  for other in $(docker network ls -q); do
    oname=$(docker network inspect -f '{{.Name}}' "$other")
    [ "$oname" = "$net" ] && continue
    osub=$(docker network inspect -f '{{range .IPAM.Config}}{{.Subnet}}{{end}}' "$other")
    if [ "$osub" = "$subnet" ]; then
      n=$(docker network inspect -f '{{len .Containers}}' "$other")
      fail "ghost network $oname also holds $subnet ($n containers attached) — see deploy/README 'Ghost network'"
    fi
  done
fi

# 7. API health
body=$(curl -fsS -m 5 "http://127.0.0.1:$PORT/api/v1/health" 2>/dev/null)
case "$body" in
  *'"status":"ok"'*) pass "/api/v1/health on :$PORT -> $body" ;;
  *) fail "/api/v1/health on :$PORT did not answer ok (got: ${body:-nothing})" ;;
esac

# 8. egress (opt-in, VPN stacks only)
if [ "$EGRESS" = 1 ]; then
  if [ "$PROJECT" = skadi-lab ]; then
    skip "egress check: the lab has no VPN"
  else
    w="${PROJECT}-skadi-downloader-worker-1"
    wip=$(docker exec "$w" curl -s -m 10 ifconfig.me 2>/dev/null)
    hip=$(curl -s -m 10 ifconfig.me 2>/dev/null)
    if [ -n "$wip" ] && [ -n "$hip" ] && [ "$wip" != "$hip" ]; then pass "worker egress differs from host egress"; else fail "worker egress '$wip' vs host '$hip'"; fi
  fi
fi

echo "$fails check(s) failed"
exit "$fails"
