#!/usr/bin/env python3
"""Delete library files a remux cannot fix, and requeue them for re-acquisition.

SKADI-T-0588. A bad container is repackable when the streams inside are fine
(see `remux-on-nas.py`). These are the ones where they are not:

  * video the device will not decode whatever the container — mpeg4/Xvid,
    MS-MPEG-4, VC-1;
  * no decodable audio track at all — DTS/TrueHD only, which plays silent;
  * or a bitrate too high to stream, whatever the container.

The third reason is the one that is easy to get wrong. A remux copies the video
through untouched, which is its virtue — and which also means it copies an 80
Mbps Blu-ray bitrate through untouched. Repacking those produces a nicely
seekable file the link still cannot carry. Measured on this library, 24 of the
29 technically-remuxable files fell into that case, so "what is repackable" and
"what ends up streamable" are different questions and only the second one
matters here.

The only fix for any of these is a different release.

Deletes run **on the NAS over ssh**, not through the daemon's NFS mount. Unlink is
a constant-time operation either way, but going direct avoids the mount's RPC
queue, which is contended by the media scan.

Per file: unlink on the NAS, then POST `/editions/{eid}/reset` so the edition
returns to `Missing`. These items are monitored, so **the next sweep will search
for and grab a replacement** — deleting N files starts N downloads.

IRREVERSIBLE while `import.recycle_bin` is unset (it is).

    python3 deploy/cull-unfixable.py                 # dry run
    python3 deploy/cull-unfixable.py --apply
    python3 deploy/cull-unfixable.py --apply --tv    # episodes instead of movies
    python3 deploy/cull-unfixable.py --apply --limit 10
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
NAS_ENV = "deploy/.env.nas"
CONTAINER_ROOT = "/mnt/storage"
NAS_ROOT = "/volume1/media"
BAD_CONTAINERS = r"\.(m2ts|ts|avi|vob|mpg|mpeg|wmv|divx)$"
DECODABLE = "('aac','ac3','eac3','opus','flac','mp3','mp2','vorbis')"
GOOD_VIDEO = "('h264','hevc','vp9','av1')"
# Matches skadi_core::streaming::HIGH_BITRATE_KBPS. A remux preserves the
# bitrate exactly, so a Blu-ray remux at 80 Mbps becomes a nicely-seekable
# Matroska file the link still cannot carry. Measured on this library: 24 of the
# 29 "remuxable" files were over this, which is why they belong here instead.
HIGH_BITRATE_KBPS = 30000


def env(path: str, key: str) -> str:
    for line in open(path):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    sys.exit(f"no {key} in {path}")


def ssh(script: str, timeout: int = 900) -> str:
    cmd = [
        "ssh", "-p", env(NAS_ENV, "NAS_SSH_PORT"),
        "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
        f'{env(NAS_ENV, "NAS_SSH_USER")}@{env(NAS_ENV, "NAS_SSH_HOST")}',
    ]
    r = subprocess.run(cmd + [script], capture_output=True, text=True, timeout=timeout)
    return (r.stdout + r.stderr).strip()


def psql(sql: str) -> str:
    user = env(ENV, "POSTGRES_USER") or "skadi"
    r = subprocess.run(
        ["docker", "exec", POSTGRES, "psql", "-U", user, "-d", "skadi",
         "-t", "-A", "-F", "\t", "-c", sql],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        sys.exit("psql failed: " + r.stderr.strip())
    return r.stdout


def candidates(tv: bool) -> list[tuple[str, str, str, str, str]]:
    """(row_id, parent_id, title, path, why) for unfixable files."""
    if tv:
        frm = "episodes e join series p on p.id = e.series_id"
        title = "p.title || ' ' || lpad(e.season::text,2,'0') || 'x' || lpad(e.number::text,2,'0')"
    else:
        frm = "movie_editions e join movies p on p.id = e.movie_id"
        title = "p.title"
    sql = f"""
        with c as (
          select e.id rid, p.id pid, {title} title, e.file_path fp,
                 e.media_info::jsonb mi,
                 (e.media_info::jsonb->>'size_bytes')::numeric sz
          from {frm}
          where e.status_kind = 'Imported' and e.file_path is not null
            and lower(e.file_path) ~ '{BAD_CONTAINERS}'
            and e.media_info is not null
            and jsonb_array_length(coalesce(e.media_info::jsonb->'audio_tracks','[]'::jsonb)) > 0
        )
        select rid, pid, title, fp,
               case when mi->'video'->>'codec' not in {GOOD_VIDEO}
                    then coalesce(mi->'video'->>'codec','?') || ' video'
                    when not exists (select 1 from jsonb_array_elements(mi->'audio_tracks') t
                                     where t->>'codec' in {DECODABLE})
                    then 'no decodable audio'
                    else (mi->>'overall_bitrate_kbps') || ' kbps' end
        from c
        where mi->'video'->>'codec' not in {GOOD_VIDEO}
           or not exists (select 1 from jsonb_array_elements(mi->'audio_tracks') t
                          where t->>'codec' in {DECODABLE})
           or (mi->>'overall_bitrate_kbps')::int > {HIGH_BITRATE_KBPS}
        order by sz desc nulls last
    """
    rows = []
    for line in psql(sql).splitlines():
        p = line.split("\t")
        if len(p) == 5:
            rows.append(tuple(p))
    return rows


def reset(parent_id: str, row_id: str, tv: bool, key: str) -> str | None:
    path = (f"{API}/series/{parent_id}/episodes/{row_id}/reset" if tv
            else f"{API}/movies/{parent_id}/editions/{row_id}/reset")
    req = urllib.request.Request(path, method="POST", headers={"X-Api-Key": key})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            body = json.loads(r.read() or b"{}")
    except urllib.error.URLError as e:
        return f"reset request failed: {e}"
    # Movies answer {edition,status,previous}; television answers {reset,previous}.
    # Accept either, because checking only one reported a whole successful batch
    # as failed the first time this ran.
    if body.get("status") == "Missing" or body.get("reset") is True:
        return None
    return f"unexpected reset response: {body}"


def q(p: str) -> str:
    return "'" + p.replace("'", "'\\''") + "'"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--tv", action="store_true", help="episodes instead of movies")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    rows = candidates(args.tv)
    if args.limit:
        rows = rows[: args.limit]
    if not rows:
        print("nothing to do")
        return

    # Size everything in ONE ssh round trip; per-file connections dominated the
    # runtime otherwise.
    paths = [r[3].replace(CONTAINER_ROOT, NAS_ROOT, 1) for r in rows]
    sizes = ssh("\n".join(f"[ -f {q(p)} ] && stat -c %s {q(p)} || echo 0" for p in paths)).split()
    total = sum(int(s) for s in sizes if s.isdigit())

    if not args.apply:
        for (_r, _p, title, _fp, why), s in zip(rows, sizes):
            print(f"  WOULD DELETE {title[:42]:42.42} {int(s) / 1e9:6.1f} GB  ({why})")
        print(f"\nDRY RUN — {len(rows)} files, {total / 1e9:.1f} GB. Re-run with --apply.")
        return

    key = env(ENV, "SKADI_API_TOKEN")
    deleted = reset_ok = freed = 0
    problems = []
    for (rid, pid, title, fp, why), s in zip(rows, sizes):
        nas_path = fp.replace(CONTAINER_ROOT, NAS_ROOT, 1)
        if not int(s or 0):
            problems.append((title, "not on disk"))
            continue
        out = ssh(f"rm -f {q(nas_path)} && ([ -f {q(nas_path)} ] && echo STILLTHERE || echo GONE)")
        if "GONE" not in out:
            problems.append((title, f"delete failed: {out[:100]}"))
            continue
        deleted += 1
        freed += int(s)
        err = reset(pid, rid, args.tv, key)
        if err:
            problems.append((title, err))
        else:
            reset_ok += 1
        print(f"  {title[:42]:42.42} {int(s) / 1e9:6.1f} GB  deleted, reset  ({why})")

    print(f"\ndeleted {deleted}/{len(rows)}   reset {reset_ok}   freed {freed / 1e9:.1f} GB")
    if problems:
        print("problems:")
        for t, p in problems:
            print(f"   {t}: {p}")


if __name__ == "__main__":
    main()
