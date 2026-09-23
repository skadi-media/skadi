#!/usr/bin/env python3
"""Remux unstreamable containers to Matroska, on the NAS, losslessly.

SKADI-T-0588. The library holds Blu-ray stream containers (`.m2ts`, `.ts`) and
legacy `.avi` that seek badly over HTTP. Where the *streams inside* are fine —
H.264/HEVC video and at least one decodable audio track — the container is the
only problem, and `-c copy` fixes it with no re-encode and no quality loss.

WHY ON THE NAS. Measured both ways: a remux driven from the Mac reads the whole
file over NFS and writes a whole new one back, at ~1 MB/s under load — hours per
file. The same remux run on the NAS is local-to-local and measured **43 MB/s**,
roughly 40x faster, because the slow part was never the disk, it was the network
between them. This is also why re-fetching beats repacking *from the Mac* and
does not beat it here.

Per file:
  1. `ffmpeg -map 0 -c copy` into a `.remux.tmp.mkv` beside the original.
  2. Verify the output's duration matches the source (within 2s).
  3. Rename to `.mkv`, delete the original.
  4. Point skadi's row at the new file and clear `media_info` so the scan
     re-profiles it.

Step 2 is the reason this is safe to run unattended: nothing is deleted until the
replacement has been shown to hold the same runtime.

    python3 deploy/remux-on-nas.py              # dry run
    python3 deploy/remux-on-nas.py --apply
    python3 deploy/remux-on-nas.py --apply --limit 3
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys

POSTGRES = "skadi-postgres-1"
ENV = "deploy/.env"
NAS_ENV = "deploy/.env.nas"
# The library is the same bytes under two names: the daemon container sees
# /mnt/storage, the NAS itself sees /volume1/media.
CONTAINER_ROOT = "/mnt/storage"
NAS_ROOT = "/volume1/media"
# DSM ships ffmpeg 4.1.9 (2019), which cannot derive dimensions from Blu-ray
# HDMV-flavoured HEVC and so refuses to mux it ("dimensions not set") — that
# failed 27 of the first 28 files. This is a 4.5 MB static 7.1.1 build, remux-only
# (no encoders/decoders), shipped to the NAS for exactly this (SKADI-T-0588).
FFMPEG = "/volume1/docker/skadi/bin/ffmpeg7"
DURATION_SLACK_S = 2.0


def env(path: str, key: str) -> str:
    for line in open(path):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    sys.exit(f"no {key} in {path}")


def ssh_cmd() -> list[str]:
    return [
        "ssh", "-p", env(NAS_ENV, "NAS_SSH_PORT"),
        "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
        f'{env(NAS_ENV, "NAS_SSH_USER")}@{env(NAS_ENV, "NAS_SSH_HOST")}',
    ]


def nas(script: str, timeout: int = 7200) -> tuple[int, str]:
    r = subprocess.run(ssh_cmd() + [script], capture_output=True, text=True, timeout=timeout)
    return r.returncode, (r.stdout + r.stderr).strip()


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


def candidates() -> list[tuple[str, str, str, str]]:
    """Imported files in a bad container whose STREAMS are already fine.

    Excludes anything a remux cannot fix: mpeg4/VC-1 video a phone will not
    decode, and files whose only audio is DTS or TrueHD. Those need re-acquiring,
    not repacking.
    """
    sql = """
        with c as (
          select me.id eid, m.id mid, m.title, me.file_path fp, me.media_info::jsonb mi,
                 (me.media_info::jsonb->>'size_bytes')::numeric sz
          from movie_editions me join movies m on m.id = me.movie_id
          where me.status_kind = 'Imported' and me.file_path is not null
            and (lower(me.file_path) like '%.m2ts' or lower(me.file_path) like '%.ts'
                 or lower(me.file_path) like '%.avi')
            and me.media_info is not null
            and jsonb_array_length(coalesce(me.media_info::jsonb->'audio_tracks','[]'::jsonb)) > 0
        )
        select eid, mid, title, fp from c
        where mi->'video'->>'codec' in ('h264','hevc')
          and exists (select 1 from jsonb_array_elements(mi->'audio_tracks') t
                      where t->>'codec' in ('aac','ac3','eac3','opus','flac','mp3','mp2','vorbis'))
          -- AND small enough to actually stream afterwards. A remux copies the
          -- bitrate through untouched, so repacking an 80 Mbps Blu-ray produces a
          -- seekable file the link still cannot carry. Those are re-acquired
          -- instead (see cull-unfixable.py); measured, that was 24 of 29.
          and coalesce((mi->>'overall_bitrate_kbps')::int, 0) <= 30000
        order by sz nulls last
    """
    rows = []
    for line in psql(sql).splitlines():
        p = line.split("\t")
        if len(p) == 4:
            rows.append((p[0], p[1], p[2], p[3]))
    return rows


DUR = re.compile(r"Duration:\s*(\d+):(\d+):(\d+\.?\d*)")


def duration_of(nas_path: str) -> float | None:
    # No `| head` here: a closed pipe sends SIGPIPE and kills ffmpeg mid-write.
    # That is what produced a 0-byte output and a silent "success" during testing.
    code, out = nas(f"{FFMPEG} -hide_banner -nostats -i {sh(nas_path)} 2>&1 || true", timeout=900)
    m = DUR.search(out)
    if not m:
        return None
    h, mi, s = m.groups()
    return int(h) * 3600 + int(mi) * 60 + float(s)


def sh(p: str) -> str:
    """Single-quote a path for a remote shell. Library paths contain (), {} and
    spaces, all of which the shell would otherwise act on."""
    return "'" + p.replace("'", "'\\''") + "'"


def relink(eid: str, new_container_path: str) -> None:
    """Point the row at the new file and requeue it for profiling.

    Both columns must move together: `file_path` is what the scanner reads, and
    the path inside `status_json`'s `Imported` variant is what the domain treats
    as the source of truth. Updating one and not the other leaves the edition
    describing a file that no longer exists.
    """
    esc = new_container_path.replace("'", "''")
    psql(f"""
        update movie_editions
           set file_path = '{esc}',
               status_json = jsonb_set(status_json::jsonb,
                                       '{{Imported,file,path}}', to_jsonb('{esc}'::text))::json,
               media_info = null
         where id = '{eid}'
    """)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    rows = candidates()
    if args.limit:
        rows = rows[: args.limit]
    if not rows:
        print("nothing to remux")
        return

    if not args.apply:
        for _eid, _mid, title, fp in rows:
            print(f"  WOULD REMUX {title[:44]:44.44}  {fp.rsplit('.', 1)[-1]}")
        print(f"\nDRY RUN — {len(rows)} files. Re-run with --apply.")
        return

    ok = failed = 0
    for eid, _mid, title, fp in rows:
        src = fp.replace(CONTAINER_ROOT, NAS_ROOT, 1)
        stem = src.rsplit(".", 1)[0]
        tmp, dst = stem + ".remux.tmp.mkv", stem + ".mkv"

        before = duration_of(src)
        if before is None:
            print(f"  SKIP  {title[:44]:44.44}  cannot read source duration")
            failed += 1
            continue

        # `-map 0` keeps every stream, subtitles included. `+genpts` because
        # transport streams often carry no usable presentation timestamps, which
        # is what makes them seek badly in the first place.
        # `-map 0:v -map 0:a -map 0:s?` rather than `-map 0`: transport streams
        # carry data tracks (timed_id3 and friends) that Matroska cannot hold, and
        # mapping them fails the whole mux. Subtitles are optional (`?`) so a file
        # without them is not an error — and PGS copies through fine.
        #
        # The big probe is not optional either: these are Blu-ray streams with no
        # global header, and without reading far enough ffmpeg cannot determine
        # dimensions or sample rates, which the muxer then rejects.
        #
        # `-nostats` because the progress spinner emits a line per update and this
        # output is captured, not watched.
        code, out = nas(
            f"{FFMPEG} -nostdin -nostats -v error -y "
            f"-probesize 200M -analyzeduration 200M -fflags +genpts -i {sh(src)} "
            f"-map 0:v -map 0:a -map 0:s? -c copy -max_interleave_delta 0 {sh(tmp)}"
        )
        if code != 0:
            print(f"  FAIL  {title[:44]:44.44}  ffmpeg: {out[:120]}")
            nas(f"rm -f {sh(tmp)}")
            failed += 1
            continue

        after = duration_of(tmp)
        if after is None or abs(after - before) > DURATION_SLACK_S:
            # The guard that makes this safe to leave running: a truncated remux
            # must never replace a complete file.
            print(f"  FAIL  {title[:44]:44.44}  duration {before:.1f}s -> {after}")
            nas(f"rm -f {sh(tmp)}")
            failed += 1
            continue

        code, out = nas(f"mv {sh(tmp)} {sh(dst)} && rm -f {sh(src)} && echo done")
        if "done" not in out:
            print(f"  FAIL  {title[:44]:44.44}  replace: {out[:120]}")
            failed += 1
            continue

        relink(eid, dst.replace(NAS_ROOT, CONTAINER_ROOT, 1))
        ok += 1
        print(f"  OK    {title[:44]:44.44}  {before / 60:.0f} min")

    print(f"\nremuxed {ok}/{len(rows)}, failed {failed}")


if __name__ == "__main__":
    main()
