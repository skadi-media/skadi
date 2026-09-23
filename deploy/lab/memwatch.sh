#!/bin/sh
# Sample a container's process memory / threads / fds every N seconds (SKADI-T-0392).
#
#   deploy/lab/memwatch.sh [container=skadi-lab-skadi-downloader-worker-1] [interval=30] [out=-]
#
# Columns: utc, rss_mib, hwm_mib, anon_mib (cgroup), file_mib (cgroup page cache),
# threads, fds, live (librqbit torrents the worker has logged as live so far is NOT
# known here — correlate with `docker logs`). Works on any container whose PID 1 is
# the process of interest; reads /proc from inside so it needs no host tools.
set -u
C=${1:-skadi-lab-skadi-downloader-worker-1}
N=${2:-30}
OUT=${3:--}
echo "utc,rss_mib,hwm_mib,anon_mib,file_mib,threads,fds" | { [ "$OUT" = - ] && cat || tee "$OUT"; }
while :; do
  line=$(docker exec "$C" sh -c '
    rss=$(awk "/VmRSS/{print int(\$2/1024)}" /proc/1/status)
    hwm=$(awk "/VmHWM/{print int(\$2/1024)}" /proc/1/status)
    thr=$(awk "/Threads/{print \$2}" /proc/1/status)
    anon=$(awk "/^anon /{print int(\$2/1048576)}" /sys/fs/cgroup/memory.stat 2>/dev/null)
    file=$(awk "/^file /{print int(\$2/1048576)}" /sys/fs/cgroup/memory.stat 2>/dev/null)
    fds=$(ls /proc/1/fd | wc -l | tr -d " ")
    echo "$rss,$hwm,${anon:-},${file:-},$thr,$fds"' 2>/dev/null) || line=",,,,,"
  echo "$(date -u +%H:%M:%S),$line" | { [ "$OUT" = - ] && cat || tee -a "$OUT"; }
  sleep "$N"
done
