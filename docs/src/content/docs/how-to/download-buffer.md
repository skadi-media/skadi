---
title: Download buffer
description: Let the download worker download and seed on a separate folder, and copy finished files to the library in one write.
---

A torrent writes a file in many small pieces, in whatever order its peers send
them. On a full or busy library volume such a file ends up in hundreds or
thousands of pieces, and the downloads compete with playback for the disks.

With a **download buffer** the worker downloads and seeds in a separate folder.
When a download is finished, the worker copies its files to `downloads/complete`
on the library volume in one sequential write, then reports it complete; the
importer hard-links from there as before. The library gets each file in one
pass, and the torrent goes on seeding from the buffer copy.

The buffer is off by default. Without it, nothing changes.

## Turn it on

In `deploy/.env`:

```sh
# A host folder for the buffer; the worker sees it at /buffer.
SKADI_BUFFER_SOURCE=/srv/skadi-buffer
# Optional limits, in GB (decimal). Empty uses the default.
SKADI_WORKER_BUFFER_MAX_GB=      # size cap, default 100; 0 = no cap
SKADI_WORKER_BUFFER_MIN_FREE_GB= # free space to keep, default 20; 0 = no floor
```

Create the folder first, writable by the worker's `PUID:PGID`, then recreate
the worker container. Only the worker mounts the buffer; the server does not.

- New downloads download and seed in the buffer. Downloads that started on the
  library volume finish there.
- The worker's session state moves to the buffer. On the first start it is
  copied from the old session folder, so existing seeds are not checked again.
- A copy is written under a temporary name beside its target, checked by size,
  then renamed. One copy runs at a time. A restart in the middle of a copy
  starts that copy again.

## Limits

The buffer has a **size cap** (what its downloads hold) and a **free-space
floor** (what must stay free on its disk, counting what downloads still have to
fetch). The first one reached applies.

- A new download that does not fit waits, still queued, until there is room.
- To make room, the worker evicts the oldest seeding copies first. An evicted
  torrent stops seeding; the library copy stays. Nothing that is still
  downloading or being copied is evicted.
- At the seed limit, the buffer copy is deleted; the library copy stays.
- A download that can **never** fit (bigger than the cap, or than the buffer
  with everything in it gone) does not start. Its job shows as
  `buffer_too_small` on the Downloads page, with the sizes and the limits. Raise
  the cap, free space or move the buffer, then press **Resume** to try it again.
  Its release is not blocklisted.

The worker logs the buffer's use and limits at start, when it admits a download,
and every 10 minutes.

## Where to put the buffer

Any folder the worker can write works. The trade-offs, in short:

| Buffer on | Good | Watch out |
|---|---|---|
| A local drive (bind mount) | Fast, keeps all download writes off the library | Its size limits what can seed at once |
| A separate NFS share | Uses other disks, a clean empty share stays unfragmented | Network round trips for every piece; mount it on the host first |
| A Kubernetes PVC | Fits a cluster deployment | Use `ReadWriteOnce` on fast storage; the worker pod alone mounts it |

The buffer is worth it only when it is a different disk, or at least a
different, emptier volume, than the library. On the same full volume a copy
still writes the file in one pass, but downloading still loads the library's
disks.

### A local drive

```sh
SKADI_BUFFER_SOURCE=/srv/skadi-buffer
```

On Docker Desktop, a folder on the host's own disk is a bind mount through the
VM. How its I/O compares with a named Docker volume inside the VM is not
measured yet.

### An NFS share

Mount the share on the host (for example at `/mnt/skadi-buffer`, with
`nfsvers=4`), then point the source at the mount point:

```sh
SKADI_BUFFER_SOURCE=/mnt/skadi-buffer
```

Use a share of its own, not a folder on the library export: the point is to keep
the library's disks free of download writes.

### A Kubernetes PVC

Skadi ships no Kubernetes manifests; this is the shape for a deployment of your
own. Mount a claim into the worker container only, and set the worker setting to
the mount path:

```yaml
volumes:
  - name: download-buffer
    persistentVolumeClaim:
      claimName: skadi-download-buffer
containers:
  - name: skadi-downloader-worker
    env:
      - name: SKADI_WORKER_BUFFER_DIR
        value: /buffer
      - name: SKADI_WORKER_BUFFER_MAX_GB
        value: "100"
    volumeMounts:
      - name: download-buffer
        mountPath: /buffer
```

## Turn it off

Clear `SKADI_BUFFER_SOURCE` and recreate the worker. Jobs that were downloading
in the buffer need the buffer folder to finish: let them finish, or remove them,
before you turn it off.
