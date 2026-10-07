# Operations runbook (deploy stack)

The production appliance is the Docker Compose stack under
[`deploy/`](https://github.com/skadi-media/skadi/tree/main/deploy); its README
is the authoritative setup guide. This page is the short day-2 runbook — what
to run, what to expect, and how to recover — for the two ways the stack is
driven:

| Entry point | Compose project | What it is |
|---|---|---|
| `angreal deploy …` | `skadi` | production on the Docker Desktop host, library over NFS |
| `angreal lab …` | `skadi-lab` | isolated second copy for experiments — no VPN, local storage |

`angreal tree` lists every command. Nothing in `angreal lab` can address the
production project; `angreal lab reset` is the only `down -v` that exists.

## Health

The daemon's only health route is **`GET /api/v1/health`** (unauthenticated,
liveness only — it does not ping the database). A bare `/health` is answered by
the web UI's single-page-app fallback with `200` and `index.html`, so it proves
nothing. `/api/v1/health/checks` (bearer token required) is the diagnostics
view: one entry per check with an `id`, a `label`, a `severity`
(`ok`/`warn`/`error`), a `message`, a `remediation` (how to fix it; `null` when
ok) and `checked_at`. The route reads stored results and does not probe. The
daemon runs the checks in the background: the local checks every 30 s, the
indexers and download clients every 5 minutes. A check that has not run yet has
the severity `pending` and `checked_at: null`. To run the checks now, send
`POST /api/v1/health/checks/run` (admin only; add `?id=<check id>` for one
check).

These checks can give a warning: `disk-space` when 75 % to 90 % of the library
filesystem is used (more than 90 % is an error); an indexer or a download
client that answers in more than 20 s, or whose recent searches fail (the last
search, or one search in four); a domain worker that stopped unexpectedly one or
two times (three times is an error); and the `indexers` and `download-clients`
summaries when some of their members do not work. These conditions are errors:
no indexer, no download client, `library.root` not set (the default `/data` is
not a library that you chose), and an enabled domain with no usable folder under
the library root (`root:<domain>`).

Neither the daemon nor the worker has a compose `healthcheck`; only
gluetun, flaresolverr and postgres do — `docker compose ps` shows those three.

## Bring-up

```sh
cd deploy && cp .env.example .env && $EDITOR .env    # VPN creds + three secrets
angreal deploy up            # up -d, then waits for the daemon
angreal deploy status        # compose ps + health
angreal deploy logs -s gluetun
```

Order is enforced by `depends_on`: postgres healthy → daemon; gluetun healthy
(tunnel up) → flaresolverr + worker. First start waits on the VPN handshake
(1–2 min). The worker then **restores its librqbit session before it claims
anything** — over NFS-on-Wi-Fi that is tens of minutes (28 min for ~370
torrents was measured), on local disk about a minute. It is not hung; watch for
`librqbit session up … restored=N fastresume=true` in the worker log.

## Upgrade / redeploy

```sh
git pull
angreal deploy redeploy                              # daemon: build, force-recreate, prune builder cache
angreal deploy redeploy --service skadi-downloader-worker
```

Never restart gluetun to "fix" things — providers rate-limit reconnects and the
stack has a gentle watchdog for that. Migrations run at daemon start.

## Rollback

Keep the previous image before a risky redeploy, then swap tags back:

```sh
docker tag skadi-downloader-worker:latest skadi-downloader-worker:prev   # before
docker tag skadi-downloader-worker:prev skadi-downloader-worker:latest   # rollback
cd deploy && docker compose up -d --force-recreate skadi-downloader-worker
```

The worker's session + fastresume files live under the library, so a rollback
re-attaches seeds without a full re-hash.

## Stopping containers

Neither binary handles `SIGTERM` yet (the daemon only listens for Ctrl-C), so
`docker stop` always waits the full timeout and then kills. Use
`docker stop -t 5 <container>` for a quick, safe stop: the librqbit session file
is written incrementally and fastresume bitfields are per torrent, so a kill
loses at most the pieces in flight.

## Recovery runbooks

**"Pool overlaps with other one on this address space" on `up`.** An orphaned
network still owns the project's subnet after an interrupted teardown.
`docker network ls`, inspect the candidates, confirm the orphan has no
containers (`docker network inspect -f '{{len .Containers}}' <name>`), then
`docker network rm <name>` and re-run `up` from `deploy/`.

**Container stuck `Dead` / "removal already in progress".** `docker rm -f`
will not clear it; restart Docker Desktop (the daemon drops Dead containers on
restart). That service is down until then.

**Restarting Docker Desktop from the CLI.** Quit it, wait until `docker info`
fails *and* the Docker process has exited, then relaunch (`open -a Docker`) and
wait for `docker info` to succeed. Relaunching too early races the VM and leaves
it down — use the GUI if that happens. Containers with
`restart: unless-stopped` come back on their own; verify with
`angreal deploy status`.

**Cloudflare-fronted indexers all failing ("error decoding torrent").** The
solver is down: check `flaresolverr` in `docker compose ps`; autoheal restarts
it when its healthcheck fails, the vpn-watchdog restarts it with gluetun.

**Database backup / restore.**

```sh
cd deploy
docker compose exec -T postgres pg_dump -U skadi -Fc skadi > skadi-$(date -u +%Y%m%dT%H%M%SZ).dump
docker compose exec -T postgres pg_restore -U skadi -d skadi --clean --if-exists --no-owner < skadi-<stamp>.dump
```

Stop the daemon and worker before a restore. The `postgres-backup` sidecar also
takes a verified daily dump and keeps the newest 14.

## Lab

`angreal lab up` needs Docker running; it builds `:lab` images (or
`--from-prod` re-tags the running ones) and serves the API on
`http://127.0.0.1:8091` with the token in `deploy/.env.lab`. The three
`STORAGE_NFS_* not set` warnings it prints are harmless — the overlay replaces
those volumes. See the lab section of `deploy/README.md` for the synthetic
library and the memory knobs.
