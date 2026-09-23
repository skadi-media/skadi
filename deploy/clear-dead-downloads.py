#!/usr/bin/env python3
"""Remove downloads that are dead, and requeue what they were for.

SKADI-T-0589. The sweep awaits each acquire run to completion
(`runner.execute(...).await` inside `buffer_unordered(4)`), and the run's last
step, `monitor_release`, polls the download with `retry_attempts = 2400,
retry_delay_ms = 30000` — twenty hours.

So a download that will never finish does not merely fail late; it occupies one
of four sweep slots for up to twenty hours. Four of them freeze that domain's
sweep entirely. Observed on this library: 29 runs stuck 1-9 hours, 114 Missing
episodes unworked, and deletes that never triggered a replacement search.

"Dead" here is deliberately conservative — `downloading`, **zero** bytes per
second, and older than the grace period. A torrent that is merely slow still has
throughput; one at exactly zero for an hour has no peers and is not coming back.
Live transfers are never touched.

Per download: DELETE it, then reset the owning edition/episode to `Missing` so
the next sweep searches again, and picks a different release.

    python3 deploy/clear-dead-downloads.py           # dry run
    python3 deploy/clear-dead-downloads.py --apply
    python3 deploy/clear-dead-downloads.py --apply --min-age-hours 3
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import urllib.error
import urllib.request

POSTGRES = "skadi-postgres-1"
API = "http://localhost:8090/api/v1"
ENV = "deploy/.env"


def env(key: str) -> str:
    for line in open(ENV):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    sys.exit(f"no {key} in {ENV}")


def psql(sql: str) -> str:
    user = env("POSTGRES_USER") or "skadi"
    r = subprocess.run(
        ["docker", "exec", POSTGRES, "psql", "-U", user, "-d", "skadi",
         "-t", "-A", "-F", "\t", "-c", sql],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        sys.exit("psql failed: " + r.stderr.strip())
    return r.stdout


def api(method: str, path: str, key: str) -> tuple[int, dict]:
    req = urllib.request.Request(f"{API}{path}", method=method, headers={"X-Api-Key": key})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        return e.code, {}
    except urllib.error.URLError as e:
        return 0, {"error": str(e)}


def dead(min_age_hours: int) -> list[tuple[str, str, str]]:
    """(download_id, acquirable_ref, description) for downloads that are dead."""
    sql = f"""
        select d.id, coalesce(d.acquirable_ref,''),
               coalesce(round(100.0*d.progress_bytes/nullif(d.total_bytes,0),1)::text,'-')
                 || '% age=' || round(extract(epoch from (now()-d.created_at))/3600,1) || 'h'
        from downloads d
        where d.status = 'downloading'
          and coalesce(d.down_speed_bps, 0) = 0
          and now() - d.created_at > interval '{min_age_hours} hour'
        order by d.created_at
    """
    rows = []
    for line in psql(sql).splitlines():
        p = line.split("\t")
        if len(p) == 3:
            rows.append((p[0], p[1], p[2]))
    return rows


def stuck_items(min_age_hours: int) -> list[tuple[str, str, str, str]]:
    """(kind, parent_id, row_id, title) for items stuck at 0% for too long.

    Resolved from the ITEM side, not from the download. `downloads.acquirable_ref`
    holds the release *title*, not an id, so it cannot identify the owning row —
    and `status_json`'s release id does not join to the downloads table either.
    Asking "which items claim to be downloading and have got nowhere" needs
    neither link and is exactly the set that wants requeueing.
    """
    sql = f"""
        select 'movie', e.movie_id, e.id, m.title
        from movie_editions e join movies m on m.id = e.movie_id
        where e.status_kind = 'Downloading'
          and coalesce((e.status_json::jsonb->'Downloading'->>'progress')::float, 0) = 0
          and now() - e.updated_at > interval '{min_age_hours} hour'
        union all
        select 'tv', e.series_id, e.id,
               s.title || ' ' || lpad(e.season::text,2,'0') || 'x' || lpad(e.number::text,2,'0')
        from episodes e join series s on s.id = e.series_id
        where e.status_kind = 'Downloading'
          and coalesce((e.status_json::jsonb->'Downloading'->>'progress')::float, 0) = 0
          and now() - e.updated_at > interval '{min_age_hours} hour'
    """
    rows = []
    for line in psql(sql).splitlines():
        p = line.split("\t")
        if len(p) == 4:
            rows.append((p[0], p[1], p[2], p[3]))
    return rows


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--min-age-hours", type=int, default=1)
    args = ap.parse_args()

    downloads = dead(args.min_age_hours)
    items = stuck_items(args.min_age_hours)

    if not args.apply:
        for _id, _ref, desc in downloads:
            print(f"  WOULD CLEAR download  {desc}")
        for kind, _p, _r, title in items:
            print(f"  WOULD REQUEUE {kind:5} {title[:48]}")
        print(f"\nDRY RUN — {len(downloads)} dead downloads, {len(items)} stuck items."
              f" Re-run with --apply.")
        return

    key = env("SKADI_API_TOKEN")
    cleared = requeued = 0
    problems = []

    # Downloads first: the stuck run is what holds the sweep slot, and requeueing
    # an item whose download is still registered would just find it in flight.
    for did, _ref, desc in downloads:
        code, _ = api("DELETE", f"/downloads/{did}", key)
        if code in (200, 202, 204):
            cleared += 1
        else:
            problems.append((did[:12], f"delete returned {code}"))
    print(f"  cleared {cleared}/{len(downloads)} dead downloads")

    for kind, parent, row, title in items:
        path = (f"/series/{parent}/episodes/{row}/reset" if kind == "tv"
                else f"/movies/{parent}/editions/{row}/reset")
        code, body = api("POST", path, key)
        if body.get("status") == "Missing" or body.get("reset") is True:
            requeued += 1
            print(f"  requeued {kind:5} {title[:48]}")
        else:
            problems.append((title[:24], f"reset returned {code}"))

    print(f"\ncleared {cleared} downloads, requeued {requeued} items")
    if problems:
        print("problems:")
        for d, p in problems:
            print(f"   {d}: {p}")


if __name__ == "__main__":
    main()
