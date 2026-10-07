# Skadi deploy stack

One `docker compose up -d` brings up the whole appliance:

| Service | What | Exposure |
|---|---|---|
| `skadi` | the daemon (HTTP API **+ web UI**) | `0.0.0.0:8080` (LAN — see note) |
| `skadi-downloader-worker` | built-in torrent client (librqbit) | **no port** — lives inside gluetun, driven via the DB |
| `postgres` | backing store | compose network only |
| `gluetun` | VPN tunnel + firewall + HTTP proxy | the daemon's native indexer searches egress through it (port 8888) |
| `flaresolverr` | CloudFlare solver for native indexers | **no network of its own** — lives inside gluetun (port 8191) |

> **Indexers are native**: Skadi's built-in Cardigann engine
> replaces Prowlarr — add trackers from the catalog in the UI (**Indexers →
> "+ Add tracker"**). Their searches ride gluetun's VPN proxy; CloudFlare-gated
> ones are solved by FlareSolverr from the same VPN exit IP.

The daemon serves a web UI at **`http://<host>:8080/`** (the same origin as
the API). It binds **`0.0.0.0` by default** so phones on your LAN can pair and
download books/APK for the native player. The UI injects its API
token to any visitor that reaches it, so keep the port on your trusted home LAN
and **do not port-forward it** — on Docker Desktop `0.0.0.0` also covers
VPN/Tailscale interfaces and IPv6. Set `SKADI_BIND=127.0.0.1` in `.env` to lock
it back to this machine. It's built into the image automatically — see
[Web UI](#web-ui)
below. The first-run walkthrough can be done entirely in the browser
(Settings → providers, Config → profile/domains) instead of the CLI.

**The safety property:** the download worker shares gluetun's network namespace
(`network_mode: service:gluetun`). Its only route to the internet is the VPN
tunnel, and gluetun's firewall drops all non-VPN egress. If the VPN drops,
torrent traffic stops. This is structural — there is no client setting that can
leak around it. The **daemon stays outside** the namespace, so the UI/API and
metadata keep working during a tunnel outage.

**The download interface is the database.** The daemon and the worker never call
each other: the daemon enqueues a row in the `downloads` table, the worker (in
the VPN namespace) claims it, downloads via librqbit, and writes progress back.
So the worker publishes no port; it only reaches Postgres. You register a
built-in **skadi** downloader (no host, no secret) — see the walkthrough.

## Prerequisites

- Docker + Compose v2
- VPN credentials from a [gluetun-supported provider](https://github.com/qdm12/gluetun-wiki)
- A **writable NFS export on your NAS** for `STORAGE_NFS_ADDR`/`STORAGE_NFS_PATH`
  (downloads + library live under it, so imports hardlink instead of copying —
  see [Paths](#paths-important)). **Verify this mount before bringing the stack
  up** — it is writable.

## Setup

```sh
cd deploy
cp .env.example .env
$EDITOR .env           # fill in VPN creds, generate the three secrets
docker compose up -d --build
```

Generate the secrets the file asks for:

```sh
openssl rand -hex 16   # POSTGRES_PASSWORD
openssl rand -hex 24   # SKADI_API_TOKEN
openssl rand -hex 32   # SKADI_SECRET_KEY  (changing later orphans stored creds)
```

Watch it come up (gluetun must be healthy before the worker/flaresolverr start):

```sh
docker compose ps
docker compose logs -f gluetun                    # until "healthy"
docker compose logs -f skadi-downloader-worker    # "db agent started"
curl -s http://127.0.0.1:${SKADI_PORT:-8080}/api/v1/health   # {"status":"ok","version":"…"}

# Native-app serving (only if you ran deploy/publish-apk.sh): the APK manifest
# must resolve, not 404. A 404 here almost always means the `./apk` bind-mount
# pointed at the wrong dir — run compose from deploy/ (or set --project-directory
# to deploy/), NOT the repo root, or ./apk resolves to <repo>/apk.
curl -s http://127.0.0.1:${SKADI_PORT:-8080}/app/manifest.json   # → {"file":"skadi-N.apk",...}
```

> **Health URL.** The daemon's only health route is **`/api/v1/health`**
> (unauthenticated liveness — it does not ping the database). A bare `/health`
> is answered by the web UI's SPA fallback with `index.html` and `200`, so it
> proves nothing; `angreal deploy up` currently polls that path, so treat its
> "healthy" as "the HTTP listener is up" and confirm with the curl above.
> `SKADI_PORT` is the host port from `.env` (8080 in the example; the container
> port is always 8080).

Prefer the angreal wrappers for day-to-day work — they pin the compose file,
never `down -v`, and prune the builder cache after image builds:

```sh
angreal deploy up | down | status | logs [-s svc] | build [-s svc] | redeploy [-s svc]
```

## Verify the kill switch

```sh
# The worker's egress IP should be your VPN exit, not your home IP:
docker compose exec skadi-downloader-worker curl -s ifconfig.me

# Hard proof: stop the tunnel, egress dies entirely:
docker compose stop gluetun
docker compose exec skadi-downloader-worker curl -s -m 5 ifconfig.me   # times out
docker compose start gluetun
docker compose restart skadi-downloader-worker
```

> **Note:** if gluetun ever restarts (crash, upgrade, the test above), restart
> the worker too — a container sharing another's network namespace doesn't
> automatically rejoin it. The kill switch holds throughout (no leak); recovery
> just isn't automatic. The daemon is unaffected (it's outside the namespace).

## VPN stability — avoid rate-limiting the tunnel

NordVPN (and most providers) **rate-limit how often a key may (re)connect**. Trip
that and *every* server starts failing with `i/o timeout` (the handshake completes
but no traffic flows) — it looks like the whole region is down, but it's the **key**
being throttled, not the servers. NordLynx/WireGuard vs OpenVPN makes no difference;
the limit is account-level. Two things cause it, both avoidable:

1. **Don't churn the stack.** Each `docker compose up`/`build`/restart that touches
   gluetun re-establishes the tunnel. When iterating on the app, rebuild and restart
   **only the daemon** — `docker compose up -d skadi` (or `--build`) — which never
   touches gluetun. Avoid `down -v` and full-stack `up --build` unless you truly need
   them. Never restart gluetun in a loop to "fix" it — that's what trips the limit.

2. **gluetun's reconnect storm is the disease.** When the tunnel connects but can't
   route (a throttle, an MTU blip, a dead server), gluetun tears WireGuard down and
   reconnects every ~6s, cycling servers — ~10 reconnects/min, which is exactly what
   trips NordVPN's rate-limiter. Then every server `i/o`-times-out and it never
   recovers on its own. **This is now disabled in the compose:**
   - `HEALTH_RESTART_VPN: off` — gluetun no longer self-restarts the tunnel, so a blip
     can't snowball into a throttle. (Note: `HEALTH_VPN_DURATION_INITIAL` was removed
     from gluetun upstream — setting it does nothing.)
   - `WIREGUARD_MTU: "1320"` — avoids the "handshake completes but no traffic" silent
     failure that can start the cycle.
   - The **`vpn-watchdog`** sidecar provides recovery *gently*: after gluetun has been
     unhealthy a few minutes it does ONE clean restart of gluetun + its namespace-mates
     (flaresolverr, worker), then waits ~5 min — at most one reconnect per ~5 min, never
     a storm. So the tunnel still auto-recovers, but can't throttle itself.

**If you're already throttled** (every server `i/o`-times-out, e.g. after past churn):
the watchdog's gentle cadence lets it cool while still probing. A genuinely flagged key
recovers fastest by **regenerating the NordLynx key** at nordaccount.com (manual /
WireGuard config) and updating `WIREGUARD_PRIVATE_KEY` + `WIREGUARD_ADDRESSES` in
`.env`. Optionally broaden the server pool (drop `SERVER_CITIES`, keep
`SERVER_COUNTRIES`). gluetun's `:v3` tag floats; the `iptables-nft` change in v3.39+
breaks WireGuard on **old kernels** (e.g. Synology 4.4.x) — if you run on such a host,
pin `qmcgaw/gluetun:v3.38.0` (modern kernels are unaffected).

## First-run walkthrough

All commands talk to the API; export once:

```sh
export SKADI_API_URL=http://127.0.0.1:8080
export SKADI_API_TOKEN=<value from .env>
SKADI=skadi   # or: alias skadi='docker compose exec skadi skadi'
```

1. **The downloader.** Register the built-in **skadi** downloader — the
   worker is already running in the VPN namespace and draining the queue. No
   host, no password:

   ```sh
   skadi settings add downloaders '{
     "kind": "skadi",
     "name": "built-in"
   }'
   skadi settings test downloaders <id-from-create>   # round-trips the queue
   ```

   > There is no other download client to choose. The built-in worker is the
   > only one, which is what lets the stack guarantee that every byte of torrent
   > traffic rides the tunnel.

2. **An indexer** — add a native tracker from the catalog in the UI (Indexers →
   "+ Add tracker"), or register a Torznab endpoint via the CLI:

   ```sh
   skadi settings add indexers '{
     "kind": "torznab",
     "name": "my-indexer",
     "base_url": "https://<indexer-host>",
     "categories": [2000],
     "api_key": "<key>"
   }'
   skadi settings test indexers <id>
   ```

3. **Profile, enable movies:**

   ```sh
   skadi settings add profiles '{ "name": "default" }'
   skadi domain enable movies
   ```

   A name-only profile is stored as the **Standard** profile (720p and up,
   cutoff Bluray-1080p, upgrades on). Pass `allowed`/`cutoff` as quality
   names (e.g. `"cutoff": "Bluray-1080p"`) or ids to customize; invalid
   combinations are rejected at create time.

   > There is **no root folder to register**: skadi owns the
   > layout under its dedicated library root (`SKADI_LIBRARY_ROOT`,
   > `/mnt/storage/skadi` by default) — movies land in `/mnt/storage/skadi/movie`,
   > TV in `.../television/`, audiobooks in `.../audiobook/`.

4. **Add a movie — the search starts immediately:**

   ```sh
   skadi add-movie --tmdb-id 603     # the profile defaults to the registered one
   skadi activity                    # watch the acquire run
   skadi library                     # imported!
   ```

   Add `--no-search` to register without searching; `skadi acquire
   <movie-id> <edition-id>` re-triggers manually any time.

   Under the hood: `acquire` enqueues a `downloads` row → the worker claims it
   and downloads over the VPN into `/mnt/storage/skadi/downloads/complete` → the importer
   hardlinks the completed file into `/mnt/storage/skadi/movie`. Follow the worker with
   `docker compose logs -f skadi-downloader-worker`.

   The supervisor reconciles provider settings every ~5s — changes via
   `skadi settings …` go live without restarting anything.

## Paths (important)

The **`storage`** NFS volume (writable, on your NAS at
`STORAGE_NFS_ADDR:STORAGE_NFS_PATH`) is mounted at **`/mnt/storage` in both the
worker and skadi**. skadi writes **only** under its **dedicated library root**,
`SKADI_LIBRARY_ROOT` (default **`/mnt/storage/skadi`**) — a skadi-only subfolder,
so on a **shared NAS** it never touches your other folders. skadi owns the layout
under *its* root; there is no root folder to register:

```
/mnt/storage/              ← your shared NAS export (your movies/, downloads/, … untouched)
  skadi/                   ← SKADI_LIBRARY_ROOT — skadi owns ONLY this
    movie/   television/   audiobook/      ← domains
    downloads/complete/    ← torrents download + seed here
```

Because downloads and the library live under one NFS export, the importer
**hardlinks** the completed file into its domain folder: instant, **no extra
disk**, and seeding continues from `downloads/complete`.

> **Create `SKADI_LIBRARY_ROOT` first** — make an empty, skadi-only subfolder on
> the NAS (e.g. `/volume1/media/skadi`) and verify the mount before you `up`. On a
> shared NAS, **never point it at a folder another app writes to** (downloads,
> media libraries): skadi treats everything under its root as its own.

This is the fix for a real incident: with downloads and media on *separate*
mounts, a completed file is on a different device than the library, so the
importer can't hardlink and falls back to a full **copy** — doubling disk use
and, on a space-limited host, filling it and crashing Docker. Keep everything
under the one `storage` mount and that can't happen.

### Adopting an existing library (migration)

Import is now a **reorg-via-link move**: each matched file is
hardlinked into the canonical `movie/`·`television/`·`audiobook/` path and the
**old path is dropped** (same export ⇒ a free move, no copy). So adopt your
existing library by scanning it **through the writable `/mnt/storage` mount** —
e.g. point Import at your old `/mnt/storage/movies` and it relinks each title into
`/mnt/storage/skadi/movie`, removing the stale folder as it goes.

> The optional read-only `/library` mount is **not** suitable for adoption now —
> a move has to delete the source, which a `:ro` mount forbids. Mount your media
> writable under `/mnt/storage` to adopt it; use `/library:ro` only for browsing.

> **Migrating from an older stack** that registered `root_folders` (e.g.
> `/mnt/storage/movies`): drop the registration — set `STORAGE_NFS_ADDR`/
> `STORAGE_NFS_PATH` + `SKADI_LIBRARY_ROOT`, bring the stack up, then adopt the
> old library via Import as above.

### Verify hardlinking

After an import, the downloaded file and the library file should share an inode
(link count ≥ 2), proving no copy was made:

```sh
docker compose exec skadi sh -c \
  'ls -li /mnt/storage/skadi/downloads/complete/**/*.mkv /mnt/storage/skadi/movie/**/*.mkv 2>/dev/null'
# the leading inode numbers match; `stat -c %h` on either shows 2+ links
```

## Metadata

With no `SKADI_TMDB_API_KEY`, Skadi uses Servarr's public metadata API
keylessly. Setting the key switches to TMDB directly.

## Web UI

The Leptos front-end (`crates/skadi-web`) is a WASM single-page app served by the
daemon itself at `/`. There is no separate web server and no extra port, and no
login screen.

**Auth.** This stack sets `SKADI_API_TOKEN`, so the JSON API requires a bearer
token. The daemon injects that token into the `index.html` it serves (into a
`<meta name="skadi-api-token">` tag), and the UI reads it and sends it on every
call — so the bundled UI just works, with the token configured purely via the
environment and a single cold start (no post-boot wiring). Leave `SKADI_API_TOKEN`
empty to run fully open. Note that injecting the token means any browser that can
load `/` receives it; that's acceptable here because the stack is bound to
loopback and never internet-exposed. Direct API calls that don't load the served
`index.html` still need the token (they get `401` without it).

**Production (this stack).** Nothing to do — `docker compose build skadi` builds
the UI as part of the image: the builder stage runs `trunk build --release` and
compiles the daemon with `--features embed-ui`, which embeds the bundle into the
binary. Just open `http://127.0.0.1:8080/`.

**Local binary (no Docker).** The UI is behind a cargo feature so a plain
`cargo build` needs no wasm toolchain. To produce a binary with the UI embedded:

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked          # or: cargo binstall trunk
cd crates/skadi-web && trunk build --release && cd -
cargo run -p skadi-cli --features embed-ui -- run
```

Without `--features embed-ui` the daemon still runs and serves the full JSON API;
`/` just shows a placeholder noting the UI wasn't compiled in.

**Development (hot reload).** Run the daemon however you like (the compose stack,
or `cargo run -p skadi-cli -- run`), then serve the UI from trunk with the API
proxied through it:

```sh
cd crates/skadi-web
trunk serve --proxy-backend=http://127.0.0.1:8080/api
```

Trunk serves the app on its own port (default `http://127.0.0.1:8080` → it picks
the next free one if 8080 is taken; pass `--port` to choose) and proxies `/api`
to the running daemon, so edits rebuild and reload in the browser without
touching the daemon. The UI uses relative `/api/v1` URLs and no token, so the
proxy is all it needs.

## Upgrading

```sh
git pull
docker compose build skadi skadi-downloader-worker
docker compose up -d
```

Database migrations run automatically at startup (the daemon owns the schema;
the worker only reads/writes the `downloads` table). The web UI is rebuilt and
re-embedded as part of `docker compose build skadi`.

The equivalent one-service flow (build → force-recreate → builder prune) is
`angreal deploy redeploy [--service skadi-downloader-worker]`. It never touches
gluetun, so the tunnel is not re-established (see "VPN stability").

### Rollback

Keep the outgoing image before a risky redeploy and swap tags back if needed:

```sh
docker tag skadi-downloader-worker:latest skadi-downloader-worker:prev   # before the redeploy
docker tag skadi-downloader-worker:prev   skadi-downloader-worker:latest # to roll back
docker compose up -d --force-recreate skadi-downloader-worker            # from deploy/
```

The worker's librqbit session + fastresume files live under the library
(`downloads/.rqbit-session`), not in the image or a volume, so a rollback
re-attaches seeds without a full re-hash.

## Operations runbooks (Docker Desktop host)

**Stopping.** Neither the daemon nor the worker handles `SIGTERM` yet (the
daemon only listens for Ctrl-C), so `docker stop` always waits the full timeout
and then `SIGKILL`s. Use `docker stop -t 5 <container>` for a quick, safe stop:
librqbit writes its session file incrementally and fastresume bitfields are per
torrent, so a kill loses at most the pieces in flight. `docker compose restart`
and `docker system df` can hang for a long time while the VM is IO-bound (e.g.
the worker hashing over NFS); `docker stop -t 5` + `docker start` work.

**Worker restart timing.** After a (re)create the worker re-adds every persisted
torrent *before* it claims anything. Over NFS-on-Wi-Fi that is tens of minutes
(28 min for ~370 torrents, ~4.5 s per torrent of file opens); on local disk
about a minute. It is not hung — wait for
`librqbit session up … restored=N fastresume=true` in `angreal deploy logs -s
skadi-downloader-worker`, then `re-attached seed … via="session"` lines.

**Ghost network — "Pool overlaps with other one on this address space".** An
orphaned network still owns `COMPOSE_SUBNET` after an interrupted teardown.
Find it, make sure nothing is attached, remove it, retry:

```sh
docker network ls
docker network inspect -f '{{.Name}} {{range .IPAM.Config}}{{.Subnet}}{{end}} containers={{len .Containers}}' $(docker network ls -q)
docker network rm <orphan>          # only when containers=0
docker compose down && docker compose up -d   # from deploy/
```

**Container stuck `Dead` / "removal already in progress".** `docker rm -f`
will not clear it and the name stays taken (seen on flaresolverr after a
10-minute `compose down`). Restart Docker Desktop — the daemon drops Dead
containers on restart. Until then that service is simply absent (for
flaresolverr: Cloudflare-fronted indexers report no results).

**Restarting Docker Desktop from the CLI.** Quit it, wait until `docker info`
fails *and* the Docker process has exited, then relaunch and wait for
`docker info` to succeed:

```sh
osascript -e 'quit app "Docker"'
while docker info >/dev/null 2>&1 || pgrep -xq Docker; do sleep 2; done
open -a Docker
until docker info >/dev/null 2>&1; do sleep 2; done
angreal deploy status
```

Relaunching before the old process has exited races the VM and can leave it
down with no error; if that happens, launch Docker Desktop from the GUI.
Containers with `restart: unless-stopped` return on their own.

**Cloudflare-fronted indexers all failing.** Symptom: grabs fail with "error
decoding torrent" (the challenge page was handed over as a `.torrent`) or every
search on those indexers returns nothing. The solver is down: check
`flaresolverr` in `docker compose ps`. autoheal restarts it when its healthcheck
fails; the vpn-watchdog restarts it together with gluetun. A burst of solves
takes tens of seconds, which is why its healthcheck allows a 30 s timeout at a
60 s interval: a tighter check got it killed mid-solve.

**Database backup / restore.** The `postgres-backup` sidecar dumps daily; a manual dump and restore:

```sh
docker compose exec -T postgres pg_dump -U ${POSTGRES_USER:-skadi} -Fc ${POSTGRES_DB:-skadi} \
  > skadi-$(date -u +%Y%m%dT%H%M%SZ).dump
# restore (stop skadi + the worker first):
docker compose exec -T postgres pg_restore -U skadi -d skadi --clean --if-exists --no-owner --no-privileges \
  < skadi-<stamp>.dump
```

**Disk hygiene.** Every container's logs are capped (10 MB × 3) in the compose
file; image builds are followed by `docker builder prune -af` in the angreal
tasks. A full Docker VM disk crashes postgres — if `/health` answers but pages
hang, check `docker system df` first.

**Read-only smoke checklist.** `deploy/lab/ops-smoke.sh` (proposal; defaults to
the lab project) inspects running state, healthchecks, OOM kills, log caps,
ghost networks and `/api/v1/health` without changing anything.

## Memory budget and fuses

Three services carry a `mem_limit` in the base file. They are **fuses, not
tuning**: each turns a leak into one container restarting with an obvious cause,
instead of a large host swapping while every service quietly gets worse. The Mac
has 23 GB, which makes an unbounded leak *slower* to notice, not less damaging —
gluetun's cap was added reactively only after a v3 leak reached 15 GB and starved
Postgres out of memory.

| service | cap | env | measured 2026-09-10 |
|---|---|---|---|
| `skadi-downloader-worker` | 1g | `MEM_WORKER` | ~535 MiB with 371 torrents restored |
| `flaresolverr` | 1536m | `MEM_FLARESOLVERR` | ~846 MiB steady, 993 MiB mid-solve |
| `gluetun` | 512m | — | ~96 MiB |

Two things worth knowing before changing these:

- **FlareSolverr's cap was sized from measurement on this host**, where the
  container is routinely around 850 MiB; a 512m cap would OOM it on sight.
- **The worker's 1g is a fuse.** A previous
  leak was a worker that reached ~9 GB; at ~535 MiB with a full library there is
  roughly 2× headroom, which is enough to ride out a burst and tight enough that
  a genuine regression trips it rather than hiding.

`skadi` (~600 MiB) and `postgres` (~250 MiB) are uncapped in the base file.
Neither has misbehaved, and Postgres in particular manages its own buffers — a
container cap set below what its `shared_buffers` expects converts a tuning
mistake into an OOM kill.

Applying a change to these means recreating the affected containers, so do it
when the worker is not mid-restore.

## Database backup and restore

The `postgres-backup` sidecar takes a `pg_dump -Fc` on a timer and prunes old
dumps. It runs by default with the rest of the stack — the library rows (what is
owned, what is wanted, every quality decision and blocklist judgement) are the
part that cannot be re-downloaded.

| knob | default | meaning |
|---|---|---|
| `BACKUP_INTERVAL` | `86400` | seconds between dumps |
| `BACKUP_KEEP` | `14` | how many verified dumps to retain |
| `BACKUP_DIR` | `/mnt/storage/skadi/backups/postgres` | on the library share, **not** a Docker volume |

Two properties worth knowing, because they are the difference between having
backups and believing you do:

- Every dump is written as `.partial` and **verified with `pg_restore -l`** before
  being renamed into place. A truncated or corrupt archive is discarded and
  logged, not kept. A dump interrupted by a restart never looks like a good one.
- Retention runs **after** a successful dump and only counts verified files, so a
  run of failures cannot prune the last good backup to make room for bad ones.

Check what you have:

```sh
docker exec skadi-postgres-backup-1 ls -lt /mnt/storage/skadi/backups/postgres
docker logs --tail 20 skadi-postgres-backup-1
```

### Restoring

The database holds live state, so restoring is a stop-restore-start, not a
hot swap. Stop the writers first — leaving skadi or the worker running during a
`--clean` restore means they write into tables that are being dropped.

```sh
# 1. Stop the writers. Postgres stays up; it is the restore target.
docker stop -t 5 skadi-skadi-1 skadi-skadi-downloader-worker-1

# 2. Pick a dump. They are timestamped UTC, newest last.
docker exec skadi-postgres-backup-1 ls -1t /mnt/storage/skadi/backups/postgres

# 3. Restore it. --clean --if-exists drops the existing objects first, so this
#    replaces the database rather than merging into it.
docker exec -i skadi-postgres-1 \
  pg_restore -U skadi -d skadi --clean --if-exists \
  < /path/on/the/share/skadi-<stamp>.dump

# 4. Start the writers again.
docker start skadi-skadi-1 skadi-skadi-downloader-worker-1
```

Then confirm the restore landed before trusting it — a restore that half-worked
is worse than one that failed, because nothing tells you:

```sh
docker exec skadi-postgres-1 psql -U skadi -d skadi \
  -c "select status, count(*) from downloads group by 1 order by 1;"
curl -fsS http://127.0.0.1:8090/api/v1/health/ready
```

Migrations are already applied inside the dump, so skadi finds the schema at the
version it was dumped at. Restoring a dump **older than the running binary** is
fine — skadi migrates it forward on start. Restoring one **newer** than the
binary is not: downgrade the image to match, or the daemon will refuse a schema
it does not know.

This is the raw-database half. The app-level config backup (settings, indexers,
profiles, via the API) is separate — restoring both is what
reconstructs a full install.

## Lab stack (isolated copy for experiments)

`angreal lab up` runs a second, independent copy of this stack as compose project
`skadi-lab`: `docker-compose.lab.yml` overlays the same base file with its own
subnet, volumes, `:lab` image tags, local storage under `deploy/.lab/storage`,
the API on `http://127.0.0.1:8091` and postgres on `127.0.0.1:5434`, using
`deploy/.env.lab` (throwaway token, no secrets). The prod `skadi` project is
never addressed by any `angreal lab` command; `angreal lab reset` is the only
place a `down -v` exists and it is pinned to `skadi-lab`.

**No VPN in the lab.** gluetun, flaresolverr, autoheal and the vpn-watchdog sit
behind a `vpn` profile and are not started, so the lab worker has NO kill switch
and the DHT is disabled (`LAB_DISABLE_DHT=1`). Use it for local / synthetic
torrents only — `cargo run --example synth_library` in the worker crate makes 300
of them plus a `seed-rows.sql` to load. Knobs: `LAB_BLOCKING_BURST=N` fires N
blocking-pool jobs at worker start (fault injection);
`deploy/lab/memwatch.sh <container> <secs> <csv>` samples RSS/threads/fds.
`--from-prod` tags the running prod images as `:lab` instead of building (tag
BOTH images first, or compose silently starts a full build for the missing one).
`angreal lab reset` is destructive (lab volumes + `deploy/.lab/storage`). The
three `STORAGE_NFS_* not set` warnings every lab command prints are harmless:
the base file interpolates them before the overlay removes those volumes.
`deploy/lab/features/*.feature` hold the (unexecuted) Gherkin description of the
expected ops behaviour for this stack and the lab.

## Note

This stack is independent of the repo-root `docker-compose.yml`, which is the
ephemeral Postgres used by `angreal db` for tests. Different project name,
different volumes; they coexist.

## Remote access (Tailscale)

The stack can join a [Tailscale](https://tailscale.com) tailnet so the daemon is
reachable from anywhere without a port forward or a public server. A
`tailscale` sidecar shares the daemon container's network
namespace, so the daemon's port 8080 answers at the node's tailnet address;
WireGuard encrypts it and Tailscale punches through both NATs, falling back to
a relay when it cannot.

1. In the Tailscale admin console generate an auth key (Settings → Keys) and
   put it in `deploy/.env` as `TAILSCALE_AUTHKEY`. State persists in the
   `tailscale-state` volume, so a one-off key is enough.
2. Set `COMPOSE_PROFILES=tailscale` in `deploy/.env` and run `angreal deploy up`.
3. In the admin console, disable key expiry for the `skadi` node.
4. Install Tailscale on the phone and sign in to the same tailnet. In the
   Skadi app, pair by entering `skadi:8080` (MagicDNS) or the node's
   `100.x.y.z:8080` address. Traffic is encrypted by WireGuard, so the plain
   `http://` pairing is fine.

Check the node with `docker compose logs tailscale` (look for "Success") and
`docker compose exec tailscale tailscale status`.

> **After `angreal deploy redeploy -s skadi`:** the `tailscale` sidecar shares the daemon's network namespace and is orphaned when that container is recreated (the node shows *offline* in `tailscale status`, phones on the tailnet see "Skadi offline", the LAN address still works). The redeploy task force-recreates it for you; if you recreate `skadi` any other way, run `docker compose --profile tailscale up -d --force-recreate --no-deps tailscale`. The same applies to `flaresolverr` after recreating `gluetun`.
