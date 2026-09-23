#!/usr/bin/env python3
"""Delete imported movie files in unstreamable containers and requeue them.

SKADI-T-0583 found these while profiling the library: Blu-ray stream containers
(`.m2ts`, `.ts`) and legacy `.avi`, which seek badly over HTTP, run 15-85 GB each,
and mostly carry DTS/TrueHD audio the phone cannot decode.

WHAT THIS DOES, per file:
  1. Refuses anything whose path is not one of the target extensions.
  2. Removes the file from the library share.
  3. POSTs `/editions/{eid}/reset`, putting the edition back to `Missing`.

CONSEQUENCE: these movies are monitored, so once the edition is `Missing` the
next sweep searches for and grabs a replacement. Deleting N files also starts N
downloads. This is the point — it is how a file gets replaced without needing
`regrab_unplayable`, since a `Missing` item has no held quality for `decide` to
have to beat.

THIS IS IRREVERSIBLE while `import.recycle_bin` is unset (it is): files are
removed outright, not retired. If no replacement can be sourced for a title, it
is gone.

    python3 deploy/cull-m2ts.py                 # dry run, touches nothing
    python3 deploy/cull-m2ts.py --apply         # delete
    python3 deploy/cull-m2ts.py --ext m2ts,avi  # pick the containers
    python3 deploy/cull-m2ts.py --limit 5       # smallest bite first
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import urllib.error
import urllib.request

DAEMON = "skadi-skadi-1"
POSTGRES = "skadi-postgres-1"
API = "http://localhost:8090/api/v1"
ENV = "deploy/.env"
DEFAULT_EXTS = "m2ts,ts,avi"


def env(key: str) -> str:
    for line in open(ENV):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    sys.exit(f"no {key} in {ENV}")


def sh(container: str, cmd: str) -> str:
    return subprocess.run(
        ["docker", "exec", container, "sh", "-c", cmd],
        capture_output=True,
        text=True,
    ).stdout.strip()


def candidates(exts: list[str]) -> list[tuple[str, str, str, str]]:
    """(edition_id, movie_id, title, file_path) for imported files in `exts`.

    Queried live rather than read from a file: the previous version depended on a
    scratch TSV, which meant the script only worked in the session that produced
    it, and could silently act on a stale list.
    """
    likes = " or ".join(f"lower(me.file_path) like '%.{e}'" for e in exts)
    sql = f"""
        select me.id, m.id, m.title, me.file_path
        from movie_editions me join movies m on m.id = me.movie_id
        where me.status_kind = 'Imported'
          and me.file_path is not null
          and ({likes})
        order by m.title
    """
    user = env("POSTGRES_USER") or "skadi"
    out = subprocess.run(
        ["docker", "exec", POSTGRES, "psql", "-U", user, "-d", "skadi",
         "-t", "-A", "-F", "\t", "-c", sql],
        capture_output=True, text=True,
    )
    if out.returncode != 0:
        sys.exit("query failed: " + out.stderr.strip())
    rows = []
    for line in out.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) == 4:
            rows.append(tuple(parts))
    return rows


def reset(movie_id: str, edition_id: str, key: str) -> str | None:
    """Reset an edition to Missing. Returns an error string, or None on success."""
    req = urllib.request.Request(
        f"{API}/movies/{movie_id}/editions/{edition_id}/reset",
        method="POST",
        headers={"X-Api-Key": key},
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            body = json.loads(r.read() or b"{}")
    except urllib.error.URLError as e:
        return f"reset request failed: {e}"
    # The movies endpoint answers {edition, status, previous}. It does NOT return
    # a `reset` key — the television one does, and checking for that reported all
    # 16 of the first run as failures when every one had in fact succeeded.
    if body.get("status") != "Missing":
        return f"unexpected reset response: {body}"
    return None


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true", help="actually delete (default: dry run)")
    ap.add_argument("--ext", default=DEFAULT_EXTS, help=f"containers to cull (default: {DEFAULT_EXTS})")
    ap.add_argument("--limit", type=int, default=0, help="only the N smallest (default: all)")
    args = ap.parse_args()

    exts = [e.strip().lstrip(".").lower() for e in args.ext.split(",") if e.strip()]
    if not exts:
        sys.exit("--ext must name at least one container")
    key = env("SKADI_API_TOKEN")

    rows = candidates(exts)
    if not rows:
        print(f"nothing to do: no imported files in {exts}")
        return

    # Size every candidate up front so --limit can take the smallest first and the
    # dry run can state the real total.
    sized = []
    for eid, mid, title, fpath in rows:
        if not any(fpath.lower().endswith("." + e) for e in exts):
            continue  # belt and braces; the query already filtered
        n = int(sh(DAEMON, f"stat -c %s '{fpath}' 2>/dev/null || echo 0") or 0)
        if n == 0:
            print(f"  skip (not on disk)  {title[:40]}")
            continue
        sized.append((n, eid, mid, title, fpath))
    sized.sort()
    if args.limit:
        sized = sized[: args.limit]

    total = sum(s[0] for s in sized)
    if not args.apply:
        for n, _eid, _mid, title, _f in sized:
            print(f"  WOULD DELETE {title[:40]:40.40}  {n / 1e9:6.1f} GB")
        print(f"\nDRY RUN — {len(sized)} files, {total / 1e9:.1f} GB. Re-run with --apply.")
        return

    deleted = reset_ok = freed = 0
    problems = []
    for n, eid, mid, title, fpath in sized:
        # Delete first, then reset: the other order lets the sweep start a
        # download while the old file is still on disk.
        state = sh(DAEMON, f"rm -f '{fpath}' && ([ -f '{fpath}' ] && echo STILLTHERE || echo GONE)")
        if state != "GONE":
            problems.append((title, f"delete failed: {state}"))
            continue
        deleted += 1
        freed += n
        err = reset(mid, eid, key)
        if err:
            # The file is already gone; a failed reset leaves the edition pointing
            # at a missing path, which is fixable by re-running. Losing the rest
            # of the batch to one HTTP error is not.
            problems.append((title, err))
        else:
            reset_ok += 1
        print(f"  {title[:40]:40.40}  {n / 1e9:6.1f} GB  deleted, reset")

    print(f"\ndeleted {deleted}/{len(sized)}   reset to Missing {reset_ok}   freed {freed / 1e9:.1f} GB")
    if problems:
        print("problems:")
        for t, p in problems:
            print(f"   {t}: {p}")


if __name__ == "__main__":
    main()
