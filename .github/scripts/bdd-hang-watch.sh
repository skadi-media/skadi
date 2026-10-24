#!/usr/bin/env bash
# Run a command; while it runs, any BDD runner (target/.../deps/bdd-*) older
# than LIMIT seconds gets every thread's backtrace from gdb and is killed, so
# its test fails at once with the evidence (SKADI-T-0747).
#
# A BDD binary finishes in seconds. The skadi-http suite hung twice on the
# runner until the job's 40-minute timeout, and never on a machine where
# anyone could look at it; the next hang leaves its stacks in the log.
#
# Usage: bdd-hang-watch.sh <command…>   (LIMIT defaults to 180)
set -uo pipefail
LIMIT="${LIMIT:-180}"

watch() {
  while true; do
    for pid in $(pgrep -f '/deps/bdd-' || true); do
      age=$(ps -o etimes= -p "$pid" 2>/dev/null | tr -d ' ')
      [ -n "$age" ] && [ "$age" -gt "$LIMIT" ] || continue
      echo "::error::BDD runner hung: $(ps -o args= -p "$pid") ran ${age}s (limit ${LIMIT}s)"
      echo "::group::thread backtraces of pid $pid"
      sudo gdb -p "$pid" -batch -ex "set pagination off" -ex "thread apply all bt" 2>&1 | head -4000
      echo "::endgroup::"
      kill -9 "$pid" 2>/dev/null
    done
    sleep 5
  done
}

watch &
watcher=$!
"$@"
rc=$?
kill "$watcher" 2>/dev/null
exit "$rc"
