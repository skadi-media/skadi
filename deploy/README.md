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
below. A first run needs no CLI steps: see [First run](#first-run).

**The safety property:** the download worker shares gluetun's network namespace
(`network_mode: service:gluetun`). Its only route to the internet is the VPN
tunnel, and gluetun's firewall drops all non-VPN egress. If the VPN drops,
torrent traffic stops. This is structural — there is no client setting that can
leak around it. The **daemon stays outside** the namespace, so the UI/API and
metadata keep working during a tunnel outage.

The `vpn` health check (System → Health, and the dashboard strip) watches this:
it is an error when gluetun does not answer or the tunnel is down, and when the
worker's egress IP differs from gluetun's exit IP. The worker reads its egress
once a minute from gluetun's control server on its own loopback
(`SKADI_WORKER_GLUETUN_URL`, default `http://127.0.0.1:8000`), which only answers
inside gluetun's namespace, so this adds no outside traffic. A worker that cannot
reach it is a warning: it is probably outside the namespace. The daemon's
`SKADI_GLUETUN_CONTROL_URL` turns the check on; the lab leaves it empty.

**The download interface is the database.** The daemon and the worker never call
each other: the daemon enqueues a row in the `downloads` table, the worker (in
the VPN namespace) claims it, downloads via librqbit, and writes progress back.
So the worker publishes no port; it only reaches Postgres. You register a
built-in **skadi** downloader (no host, no secret); the first boot registers it.

## Prerequisites

- Docker + Compose v2
- VPN credentials from a [gluetun-supported provider](https://github.com/qdm12/gluetun-wiki)
- A **writable NFS export on your NAS** for `STORAGE_NFS_ADDR`/`STORAGE_NFS_PATH`
  (downloads + library live under it, so imports hardlink instead of copying —
  see [Paths](#paths-important)). **Verify this mount before bringing the stack
  up** — it is writable.

## Setup

```sh
angreal deploy init        # writes deploy/.env with generated secrets
$EDITOR deploy/.env        # set the VPN values and the storage values
angreal deploy up          # starts the stack and waits for /api/v1/health/ready
```

`angreal deploy init` copies `.env.example` to `.env` and puts generated values
in `POSTGRES_PASSWORD`, `SKADI_API_TOKEN` and `SKADI_SECRET_KEY`. It does not
overwrite an existing `.env` or secret file: a new `POSTGRES_PASSWORD` locks the
stack out of its database. Options:

- `--secrets` writes the three secrets to files under `deploy/secrets/` and
  leaves them empty in `.env` (see [Secrets from files](#secrets-from-files)).
- `--no-default-indexers` sets `SKADI_DEFAULT_INDEXERS=false` (see below).

In `.env`, set the VPN values (`VPN_SERVICE_PROVIDER`, `OPENVPN_USER` and
`OPENVPN_PASSWORD`, or the WireGuard values) and the storage values
(`STORAGE_NFS_ADDR`, `STORAGE_NFS_PATH`, `SKADI_LIBRARY_ROOT`). Without angreal,
copy `.env.example` to `.env`, put `openssl rand -hex 16`, `-hex 24` and `-hex 32`
in the three secrets, and run `docker compose up -d --build` in `deploy/`.

The first `up` waits on gluetun's VPN healthcheck, so it can take a minute or
two. `angreal deploy status` shows the containers and the readiness probe. The
only health route is `/api/v1/health` (liveness) and `/api/v1/health/ready`
(readiness). A bare `/health` is the web UI and always answers 200.

Prefer the angreal wrappers for day-to-day work. They pin the compose file,
never `down -v`, and prune the builder cache after image builds:

```sh
angreal deploy init | up | down | status | logs [-s svc] | build [-s svc] | redeploy [-s svc]
```

If you ran `deploy/publish-apk.sh`, check that
`curl -s http://127.0.0.1:${SKADI_PORT:-8080}/app/manifest.json` gives
`{"file":"skadi-N.apk",...}`. A 404 means the `./apk` mount points at the wrong
directory: run compose from `deploy/` (or with `--project-directory deploy`).

## Secrets from files

By default the stack reads its secrets from `deploy/.env` as plain environment
variables. Docker shows those values in `docker inspect`, and every process in
the container can read them. The opt-in overlay `docker-compose.secrets.yml`
reads them from files instead (SKADI-T-0702). The containers then get only the
path of each file:

| File in `deploy/secrets/` | Replaces in `.env` | Read by |
|---|---|---|
| `postgres_password` | `POSTGRES_PASSWORD` | postgres, postgres-backup, skadi, worker |
| `skadi_api_token` | `SKADI_API_TOKEN` | skadi |
| `skadi_secret_key` | `SKADI_SECRET_KEY` | skadi |
| `tailscale_authkey` | `TAILSCALE_AUTHKEY` | tailscale (only with the `tailscale` profile) |

`deploy/secrets/` is in `.gitignore`. Do not commit it.

**How the overlay is applied.** `angreal deploy …` applies it when the directory
`deploy/secrets/` exists, and not otherwise. With plain compose, give both files:
`docker compose -f docker-compose.yml -f docker-compose.secrets.yml up -d`.
A deploy without `deploy/secrets/` does not change.

**The rule in the daemon and the worker.** Every `SKADI_*` variable `X` can also
be given as `X_FILE`, the path of a file that holds the value. One trailing
newline is removed. If you set both `X` and `X_FILE`, the process stops with an
error that names the variable (never the value). It also stops if the file
cannot be read. The database password can be kept out of the URL:
`SKADI_DATABASE_URL=postgres://skadi@postgres:5432/skadi` together with
`SKADI_DATABASE_PASSWORD_FILE=/run/secrets/postgres_password`.

### Migrating an existing `.env`

Do these steps on the host that runs the stack, in `deploy/`.

1. Make the directory, and write each value from `.env` to its file. Do not
   type the values on the command line:

   ```sh
   mkdir -m 700 secrets
   for pair in POSTGRES_PASSWORD:postgres_password SKADI_API_TOKEN:skadi_api_token \
               SKADI_SECRET_KEY:skadi_secret_key TAILSCALE_AUTHKEY:tailscale_authkey; do
     var=${pair%%:*}; file=secrets/${pair#*:}
     sed -n "s/^$var=//p" .env | tail -n 1 > "$file"
   done
   chmod 444 secrets/*
   ```

   The files must be readable by the container users (postgres is uid 999,
   skadi and the worker are `PUID`). The `700` directory keeps other host users
   out. A deploy without Tailscale can leave `tailscale_authkey` empty.

2. Check that each file holds one line with the value that you expect
   (`wc -l secrets/*` shows `1` for each).

3. Recreate the services that read secrets:

   ```sh
   angreal deploy up        # applies docker-compose.secrets.yml because secrets/ exists
   ```

4. Check that the values are not in the environment. This command must print
   nothing:

   ```sh
   docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' $(docker compose ps -q) \
     | grep -E '^(POSTGRES_PASSWORD|PGPASSWORD|SKADI_API_TOKEN|SKADI_SECRET_KEY|TS_AUTHKEY)=' \
     | grep -v '^TS_AUTHKEY=file:'
   ```

   (`TS_AUTHKEY=file:…` is a path, not the key.) Then check
   `curl -s http://127.0.0.1:${SKADI_PORT:-8080}/api/v1/health/ready`.

5. Remove the four values from `.env` (leave the lines empty, or delete them).
   Keep a copy of the values in your password manager: the files are now the
   only copy on the host.

   The ops scripts `clear-dead-downloads.py`, `cull-m2ts.py` and
   `cull-unfixable.py` read `SKADI_API_TOKEN` from `.env` and do not read the
   files yet. Keep the token in `.env` until they do, or do not run them after
   step 5.

**Rollback.** Put the values back in `.env`, move `secrets/` out of `deploy/`,
and run `angreal deploy up`. The stack is then back on plain variables.

### Rotating SKADI_SECRET_KEY

`SKADI_SECRET_KEY` encrypts the secret field of each provider in the database
(the `credentials` table): indexer API keys, download-client passwords, and
notifier secrets, with the cardigann tracker logins. It does not encrypt the API
token or any other setting. A config backup (Settings → Backup) holds these
fields still encrypted, so a backup needs the key that was current when it was
made.

To change the key, re-encrypt the stored credentials with `skadi rekey`
(SKADI-T-0521). You do not have to enter the provider secrets again.

1. Make a database backup first (see "Database backup and restore").
2. Stop the writers: `docker compose stop skadi skadi-downloader-worker`.
3. Keep the old key, and put the new key in its place.

   Secret files:

   ```sh
   cp secrets/skadi_secret_key secrets/skadi_secret_key.old
   openssl rand -hex 32 > secrets/skadi_secret_key
   ```

   Plain `.env`: write the old value down, then replace `SKADI_SECRET_KEY` in
   `.env` with the output of `openssl rand -hex 32`.

4. Re-encrypt, with the old key given as a file so that it is not on the
   command line:

   ```sh
   docker compose -f docker-compose.yml -f docker-compose.secrets.yml run --rm --no-deps \
     -v "$PWD/secrets/skadi_secret_key.old:/run/secrets/old_key:ro" \
     -e SKADI_OLD_SECRET_KEY_FILE=/run/secrets/old_key skadi rekey
   ```

   With a plain `.env`, leave out the second `-f` file, and write the old key
   to a temporary file for the `-v` mount. (`--old-key <value>` also works, but
   it puts the key in the shell history.)

   The command prints `rekeyed N credential(s); M already current; K
   unreadable`. `K` must be `0`. A row that cannot be read with either key is
   left as it was; run the command again with the correct old key.

5. Start the stack: `angreal deploy up`. The providers load, and the System page
   shows no "credential could not be read" warnings.
6. Keep `skadi_secret_key.old` (offline, not in `deploy/`) for as long as you
   keep backups made before the rotation, then delete it.

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
`SERVER_COUNTRIES`). gluetun is pinned to `v3.41.3` (see "Image pins"); the
`iptables-nft` change in v3.39+ breaks WireGuard on **old kernels** (e.g. Synology
4.4.x) — if you run on such a host, pin `qmcgaw/gluetun:v3.38.0` with its digest
instead (modern kernels are unaffected).

## First run

There are no CLI steps. On each boot the daemon adds only what is missing, and it
logs each action (`first boot: …` in `angreal deploy logs`):

| when | the daemon does |
|---|---|
| there is no download client | registers the built-in **skadi** downloader (downloads under `<SKADI_LIBRARY_ROOT>/downloads`) |
| there is no quality profile | creates the default profiles (Any, SD, HD-720p, HD-1080p, HD-720p/1080p, Ultra-HD) |
| the database is new | enables each domain (movies, TV, audiobooks) whose folder under `SKADI_LIBRARY_ROOT` exists or can be created |

An install that already has these is not changed. A domain that you disable
stays disabled after a restart. If the library root was not mounted on the
first boot, no domain is enabled: enable them on the Config page.

**Trackers.** Each boot, and each definition sync, registers every curated
public tracker that is missing: public, English, no login, with movie, TV, book
or audiobook categories, and no adult or anime trackers. An auto-registered
tracker that leaves that scope is removed. A curated tracker that you remove
comes back: more trackers help a torrent with few seeders. Trackers that you add
in the UI (**Indexers → "+ Add tracker"**) are never touched. To stop this, set
`SKADI_DEFAULT_INDEXERS=false`: then nothing is added or removed.

Then add a movie in the UI (or `skadi add-movie --tmdb-id 603`). The search
starts at once. The worker downloads over the VPN into
`<SKADI_LIBRARY_ROOT>/downloads/complete`, and the importer hardlinks the file
into `<SKADI_LIBRARY_ROOT>/movie`. skadi owns the layout under its library root:
there is no root folder to register.

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

> There is no separate read-only library mount any more (the old `nas-library`
> volume at `/library` was removed in SKADI-T-0704). An import hardlinks on the
> same mount and then deletes the source, and a second `:ro` mount allows
> neither. Put the media you want to adopt under the writable `/mnt/storage`.

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
docker tag skadi-downloader-worker:local skadi-downloader-worker:prev    # before the redeploy
docker tag skadi-downloader-worker:prev  skadi-downloader-worker:local   # to roll back
docker compose up -d --force-recreate skadi-downloader-worker            # from deploy/
```

(`:local` is the tag of an unpinned, locally built stack. A stack that runs the
published images rolls back by setting the previous release in `SKADI_IMAGE_TAG`,
then `angreal deploy pull && angreal deploy up`.)

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

## Image pins

Every third-party image in `docker-compose.yml` is pinned to a version tag plus
its multi-arch index digest, so a `pull` cannot change what runs. Set
2026-10-07 to what prod was already running:

| service | image | tag |
|---|---|---|
| gluetun | `qmcgaw/gluetun` | `v3.41.3` |
| flaresolverr | `ghcr.io/flaresolverr/flaresolverr` | `v3.4.6` |
| postgres, postgres-backup | `postgres` | `16.15` (a 16.15 build of 2026-08-25) |
| vpn-watchdog | `docker` | `29.8.0-cli` |
| autoheal | `willfarrell/autoheal` | `1.2.0` |
| tailscale | `tailscale/tailscale` | `v1.102.5` |
| tailscale-http-redirect | `nginx` | `1.27.5-alpine` |

gluetun's earlier leak (see "Memory budget and fuses") was on the floating `v3`
tag, release not recorded; keep the 512m fuse whatever you pin.

**skadi and the worker** are this repo's images. With `SKADI_IMAGE_TAG` unset they
are built from the checkout as `skadi:local` / `skadi-downloader-worker:local`
(development). To run a published release, set both `SKADI_IMAGE_PREFIX=ghcr.io/skadi-media/`
and `SKADI_IMAGE_TAG=<release>` (e.g. `0.1.4`) in `.env`; never follow `latest`.
A pinned stack rolls forward with `angreal deploy pull` + `angreal deploy up`,
not `redeploy` (which builds; see SKADI-T-0641).

### Bumping an image pin

1. Pick the release tag (read the project's release notes; for gluetun, check
   the issue tracker for memory reports first).
2. Get its multi-arch index digest — the `Digest:` line, not a per-platform one:
   `docker buildx imagetools inspect qmcgaw/gluetun:v3.42.0`
3. Write both into the `image:` line: `qmcgaw/gluetun:v3.42.0@sha256:<digest>`.
   For postgres, change `postgres` and `postgres-backup` together and stay on
   major 16 (a new major needs a dump and restore).
4. `angreal check compose`, then try it on the lab stack (`angreal lab up`)
   before prod. Bring prod over with `docker compose pull <service>` +
   `up -d <service>` — and never casually for gluetun (NordVPN rate limit; a
   recreated gluetun also needs its namespace mates recreated).
5. Update the table above.

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
`angreal lab up --secrets` writes `deploy/.lab/secrets/` from `.env.lab` and runs
the lab with `docker-compose.secrets.yml` (see "Secrets from files"). Every lab
command applies the overlay while that directory exists; delete it to go back
to plain variables.
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
