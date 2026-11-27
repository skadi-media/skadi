# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- A download buffer for the download worker, off by default: with `SKADI_WORKER_BUFFER_DIR` set, new downloads download and seed in that folder and the worker's session state lives there; a finished download is copied to the complete folder on the library volume in one sequential write before it is reported complete. Downloads already on the library volume finish there (SKADI-T-0783).

### Fixed
- An import onto a file that was already in place (the same file, or a hard link of it) left a second name `<name>.skadi-incoming` beside it. Library details counted these as files with no item. Such an import now leaves the file as it is and reports it already present (SKADI-T-0781).

## [0.1.17] - 2026-11-27

### Fixed
- The piece probe measured nothing when the first file in its queue was a large file in very many pieces: the count of such a file takes over a minute, the connection was closed after 60 seconds of silence, and the same file was tried again at every tick with no failure recorded. A count may now take 10 minutes; a file over that is left aside until the daemon starts again and the probe goes on with the next; counts that break five times in a row stop the probe with the reason, as the failure rule says; and a tick that has used its five minutes takes no new file (SKADI-T-0780).

## [0.1.16] - 2026-11-26

### Added
- A piece probe, off by default: Skadi can ask the storage host over SSH how many pieces each library file is in, which the mount cannot say. Set `storage.probe.host`, `.user` and `.path_map` in Settings → Library, store a private key or a password in Settings → Storage, then turn `storage.probe.pieces.enabled` on. It measures at most 120 files an hour, only reads, and needs `filefrag` or `python3` on the host. The host's key is kept at the first connection and a different key is refused. When the probe cannot work (login refused, key changed, no tool, wrong path map) it stops, a health check shows the reason, and it stays off until an admin clears it (SKADI-T-0775).
- Settings has a "Library details" page for the admin: files, size, space on disk, hard-linked files and gone files by kind; the free space of the library disk; when each kind was last read and whether a reading runs now; the list of every file, sorted and filtered by the daemon one page at a time; and the media files in the library folders that no item holds. Piece counts and checksums read "not measured" until their collectors exist (SKADI-T-0774).
- Skadi records the size, the space on disk, the hard links and the identity of each library file: when a file is imported, and in a pass over the library every 24 hours (`storage.sweep.interval_hours`; 0 turns it off). The pass also records the files that are gone and the media files in the library folders that no item holds. Nothing shows these yet; a "Library details" page follows (SKADI-T-0773).

### Fixed
- The VPN watchdog started a container that had been stopped on purpose: on a reconnect it restarted every container that shares the VPN's network, and a restart also starts a stopped one. It now restarts only those that were running, and names the ones it left stopped (SKADI-T-0771).
- The VPN watchdog did nothing while the VPN provider refused the login: gluetun kept retrying one server for 25 hours with the tunnel down and its container still reported healthy. The watchdog now also checks that traffic passes through the tunnel, and after three failed checks does its one clean reconnect, as it does for an unhealthy container (SKADI-T-0769).

## [0.1.15] - 2026-11-18

### Fixed
- Audiobook series packs often imported nothing for the book they were grabbed for. A file is now matched by its path below the download folder, a book titled after its series no longer ties with every file of the pack, and a pack of which the library tracks one book places that book and leaves the rest in the download. A pack that delivers other books and not the wanted one is marked failed for that book and not grabbed for it again (SKADI-T-0763).
- Audio that could not be matched was set aside in `_review` again on every repeat import, under `name.1`, `name.2` and so on, and only its last folder was kept, so discs of different books were mixed. A file already in `_review` is not linked again, and the path below the download folder is kept (SKADI-T-0763).
- The audiobook library-import scan read any ten-letter word in brackets as an ASIN, such as `(Unabridged)` (SKADI-T-0763).
- A download was given up on as "stalled at 0% for 3h", and its release blocklisted, while the download worker was stopped or still adding the torrent. A transfer whose worker has stopped renewing its claim is now reported as waiting for a worker, and a claimed transfer is not live until the worker says so; neither can stall (SKADI-T-0767).
- The download worker kept downloading a torrent after its job was removed, when the removal came while the torrent was still being added. The data stayed in the downloads folder with no job to import or delete it. The worker now drops such a torrent when it starts to track it, and every ten minutes drops any torrent in its session that has no live job. A removal that deletes data also removes the job's emptied folder (SKADI-T-0764).
- A manual import (Downloads → Import, `POST /downloads/import`) put the file in the library but left the film, episode or book Missing, so it was looked for again. The item now becomes Imported with the file's path and quality, as after an automatic import. A file that was already in place is counted too, so importing again repairs an item the old behaviour left Missing (SKADI-T-0766).
- An episode whose title starts with a number was read as a range of episodes: `S01E04 - 10.00 A.M.` matched episodes 4 to 10, so one file of a season pack was given to seven episodes. A second number is a range end only when it is tight against the dash (`E05-07`) or has its own marker (`E05 - E07`) (SKADI-T-0765).

## [0.1.14] - 2026-11-12

### Fixed
- A download waiting in the queue for a worker was given up on after 18 hours and its release blocklisted, without ever having been started. With `worker.max_active` set and many downloads queued, this blocklisted good releases and drove items down to worse ones. A download no worker has taken is now reported as waiting, and the give-up clock runs only from when a worker takes it (SKADI-T-0761).

## [0.1.13] - 2026-11-10

### Added
- Skadi for Samsung TV: a Tizen app in `clients/skadi-tizen` (COLLIERY-I-0673, SKADI-T-0758). Sign in once; Home with Continue watching, recently added and rows by genre; Movies and TV grids with a letter strip; search; a series page; a player that uses the TV's own decoder. It is installed in the TV's developer mode and is not part of the images.

### Fixed
- A Samsung TV could play a file but not skip through it. Its HTTP client reads response header names case-sensitively and did not see `accept-ranges` and `content-range`. HTTP/1.1 responses now carry Title-Case header names (SKADI-T-0756).
- The arm64 `skadi` image did not build. The web UI's lock file held wasm-bindgen 0.2.130, whose prebuilt arm64 tool needs a newer glibc than the image builder has. It is pinned to 0.2.129, the version 0.1.12 was built with (SKADI-T-0759).

### Changed
- Rust 1.95. `Cargo.lock` is committed, for the workspace and for the web and worker crates, so a new release of a dependency can no longer break CI on its own (SKADI-T-0757).

## [0.1.12] - 2026-10-25

### Added
- New imports, on the web and in the Android app (COLLIERY-I-0653). The web Overview has a New imports widget for every role: the latest 15, this week's count, and See all. `/new` lists every new import, newest first, with a media filter, an Upgrade pill and infinite scroll; a row opens its item, and an episode opens its season. Both views refresh every 30 s while the tab is visible (SKADI-T-0745). The Android app has a New tab with this week's count as its badge (SKADI-T-0746).
- `GET /new-imports?kind=&cursor=&limit=` and `GET /new-imports/count`: new imports filtered by the member's household policy, paged with a cursor, and the count of the last 7 days. Every role may read them (SKADI-T-0744).
- `acquisition_history.upgrade`: an import that replaced a file the library held is marked (SKADI-T-0743).

### Changed
- An `imported` history row now means a file new to the library. A resumed run that writes the same import again adds no second row. Library import of an upload (a file under `<library.root>/incoming`) writes a row; adopting an existing tree, and reconcile, write none (SKADI-T-0743).
- The web Overview's admin-only "Recently imported" feed is replaced by the New imports widget.
- The e2e harness registers the audiobooks library provider, as the daemon does.
- CI: a BDD runner that hangs gets its thread backtraces dumped and is killed after 3 minutes, instead of holding the job to its 40-minute timeout (SKADI-T-0747).

## [0.1.11] - 2026-10-18

### Fixed
- A kid lost an allowed audiobook when the book was deleted and added again: the list held the old book id. The list now matches a book by its id or its Audible ASIN, and a delete moves the entry to the ASIN (SKADI-T-0741).
- An audiobook download that holds the book as MP3s and as one M4B of the same length imports the M4B. It used to record the first MP3 (SKADI-T-0740).
- `angreal deploy up` and `redeploy` stop the downloader worker first and wait for it, so a slow worker stop no longer leaves the daemon down (SKADI-T-0645).

### Changed
- The M4B conversion uses an M4B already in the book's folder, when it is the same length as the other audio, instead of skipping the book. The edition moves onto it, seeding stops and the other audio is retired (SKADI-T-0139).
- CI builds and signs the Android APK, and the `skadi` image carries it, so the app update notice works without a separate publish (SKADI-T-0637).

## [0.1.10] - 2026-10-18

### Fixed
- The downloader worker grew about 29 MiB a minute until its memory cap. It now runs librqbit 9 from skadi's fork, which bounds the DHT peer-search queue (ikatson/rqbit#706) and the per-torrent peers map (ikatson/rqbit#667) (SKADI-T-0645).

### Changed
- The worker is on librqbit 9. It listens on one port (the forwarded port, else the first of the range), IPv4 only as before. The session saved by 0.1.9 restores without a full re-check.
- `angreal version bump` raises the Android app's version when the app changed since its version last rose, and bumps `aurora-css` and the e2e lockfile too.

### Added
- `import.audiobook_convert_concurrency` (default 3, 1 to 8): how many audiobooks convert to M4B at the same time. The AAC encoder uses its fast coder (SKADI-T-0139).

## [0.1.9] - 2026-10-17

### Fixed
- A multi-file audiobook reached the phone and the web player as its first part only. `/audio` now serves every MP3 part as one stream, `/chapters` lists the parts, the probed duration is the whole book, and a delete with files removes every part (SKADI-T-0738).

### Changed
- Web client on Aurora 0.6.1. Skadi's drawer, scroll lock, `<main>` id and `#main` height workarounds are gone; Skadi keeps its own skip link.
- Docs: plissken 0.2.0 (the release binary, pinned by sha256). Private items are out of the API reference; it has an overview page.
- Docs theme starlight-aurora v0.3.0.
- Docs: plissken 0.2.1 (was 0.2.0): intra-doc links resolve by the whole path (SKADI-T-0727).

### Added
- Import lists in the web UI: a Lists page under Settings creates, edits, syncs and deletes lists, and keeps exclusions; a movie or series page can exclude itself from every list (SKADI-T-0733).
- IMDb lists, fetched through FlareSolverr, and audiobook lists, added monitored by default (SKADI-T-0733).
- Android: films and episodes download for offline play, the way audiobooks do (SKADI-T-0637).
- A per-role sweep in the web e2e suite; `angreal version bump` and the release jobs stop when a page offers a role what the API refuses (SKADI-T-0639).
- List records carry sort keys (`sort_title`, `sort_name`, `sort_authors`, `sort_series`, `sort_collection`); the web and Android order by them (SKADI-T-0737).
- Optional conversion of a downloaded audiobook to one M4B with chapters (`import.audiobook_convert_to_m4b`, off by default). The image now has a small static `ffmpeg` for it. A converted book's download stops seeding and its MP3s are removed (SKADI-T-0139).
- Initial project structure
