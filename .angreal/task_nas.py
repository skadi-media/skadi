import angreal
import os
import shlex
import subprocess
import tempfile
import time
import urllib.request

cwd = os.path.join(angreal.get_root(), '..')
deploy_dir = os.path.join(cwd, "deploy")

nas = angreal.command_group(
    name="nas",
    about="build, ship and run the deploy stack ON the Synology NAS (SKADI-I-0056)",
)

# The NAS runs the production compose file + deploy/docker-compose.nas.yml (local
# bind mounts instead of NFS, mem_limits, :nas image tags). Images are built HERE
# for linux/amd64 and shipped with `docker save | ssh | docker load` — the NAS
# (4 GB RAM) never builds. Connection + site settings live in deploy/.env.nas;
# the NAS's own .env is assembled by `push` from deploy/.env + deploy/.env.nas.
#
# Nothing in this module touches the Mac's `skadi` compose project except
# `db-migrate`, which only READS it (pg_dump) and refuses while the daemon runs.
NAS_IMAGES = ("skadi", "skadi-downloader-worker")
NAS_LOCAL_TAG = "nas"  # tag for images built here + shipped with `push --images`
NAS_PROJECT = "skadi"  # compose project name on the NAS (Container Manager shows it under this name)
NAS_DOCKERFILES = {
    "skadi": os.path.join(cwd, "Dockerfile"),
    "skadi-downloader-worker": os.path.join(deploy_dir, "worker.Dockerfile"),
}
NAS_BASE_ENV = os.path.join(deploy_dir, ".env")
NAS_NAS_ENV = os.path.join(deploy_dir, ".env.nas")
NAS_COMPOSE_FILES = ("docker-compose.yml", "docker-compose.nas.yml")  # inputs, synced to <NAS_DATA_DIR>/deploy/src/


def _read_env(path):
    """Minimal KEY=value parser (comments/blank lines skipped, no interpolation)."""
    out = {}
    if not os.path.exists(path):
        return out
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def _cfg():
    c = _read_env(NAS_NAS_ENV)
    if "NAS_SSH_HOST" not in c:
        raise SystemExit(f"{NAS_NAS_ENV} is missing NAS_SSH_HOST — see docker-compose.nas.yml")
    c.setdefault("NAS_SSH_PORT", "22")
    c.setdefault("NAS_SSH_USER", os.environ.get("USER", ""))
    c.setdefault("NAS_DOCKER", "docker")
    c.setdefault("NAS_DATA_DIR", "/volume1/docker/skadi")
    return c


def _ssh_base(c):
    return ["ssh", "-p", c["NAS_SSH_PORT"], "-o", "BatchMode=yes",
            f"{c['NAS_SSH_USER']}@{c['NAS_SSH_HOST']}"]


def _ssh(c, remote_cmd, **kw):
    return subprocess.run(_ssh_base(c) + [remote_cmd], cwd=cwd, **kw)


def _remote_compose(c, args):
    """Shell string that runs `docker compose <args>` against the RENDERED
    project file on the NAS — the same one Container Manager imports."""
    return (f"cd {shlex.quote(c['NAS_DATA_DIR'])}/deploy && "
            f"{c['NAS_DOCKER']} compose -p {NAS_PROJECT} -f compose.yaml {args}")


def _remote_render(c):
    """Render src/base + src/overlay + .env into one self-contained compose.yaml
    (Container Manager's project file: no env file, no overlays, absolute paths)."""
    d = shlex.quote(c["NAS_DATA_DIR"])
    files = " ".join(f"-f src/{f}" for f in NAS_COMPOSE_FILES)
    return (f"cd {d}/deploy && umask 077 && "
            f"{c['NAS_DOCKER']} compose -p {NAS_PROJECT} --project-directory {d}/deploy "
            f"--env-file .env {files} config > compose.yaml.tmp && "
            # Container Manager sets the project name itself; a top-level `name:` would clash.
            f"grep -v '^name: ' compose.yaml.tmp > compose.yaml && rm compose.yaml.tmp && "
            f"chmod 600 compose.yaml && grep -c '^  [a-z]' compose.yaml")


def _health_url(c):
    port = _read_env(NAS_BASE_ENV).get("SKADI_PORT", "8080")
    # Readiness, not liveness (SKADI-T-0475): bare `/health` is the SPA fallback
    # and returns 200 before the daemon can do anything.
    return f"http://{c['NAS_SSH_HOST']}:{port}/api/v1/health/ready"


def _NAS_wait_healthy(url, timeout_s=300):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=3) as r:
                if r.status == 200:
                    return True
        except Exception:
            pass
        time.sleep(5)
    return False


@nas()
@angreal.command(
    name="build",
    about="build the :nas images for linux/amd64 on this machine (default both)",
    tool=angreal.ToolDescription(
        """
        `docker build --platform linux/amd64` of skadi and/or the downloader
        worker, tagged `<image>:nas`, then prune the builder cache. The NAS is
        x86_64 and can't build a Rust workspace in 4 GB, so images are built
        here (Docker Desktop's amd64 emulation) and shipped with `nas push`.

        ## Notes
        - Slow: a full amd64 workspace build is tens of minutes. Use
          `--service` to build only what changed.
        - Never touches the Mac stack's `:latest` images.
        """,
        risk_level="safe",
    ),
)
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="skadi | skadi-downloader-worker (default: both)",
)
def build(service=None):
    c = _cfg()
    targets = [service] if service else list(NAS_IMAGES)
    for img in targets:
        if img not in NAS_DOCKERFILES:
            raise SystemExit(f"unknown service {img!r}; one of {', '.join(NAS_IMAGES)}")
        print(f"==> building {img}:{NAS_LOCAL_TAG} for linux/amd64")
        rc = subprocess.run(
            ["docker", "build", "--platform", "linux/amd64",
             "-t", f"{img}:{NAS_LOCAL_TAG}", "-f", NAS_DOCKERFILES[img], "."],
            cwd=cwd,
        ).returncode
        if rc != 0:
            raise SystemExit(rc)
    # Same discipline as `angreal deploy redeploy`: builds fill the Docker VM disk.
    subprocess.run(["docker", "builder", "prune", "-af"], cwd=cwd)
    return 0


@nas()
@angreal.command(
    name="push",
    about="sync compose sources + .env + apk/ to the NAS and render its self-contained compose.yaml",
    tool=angreal.ToolDescription(
        """
        1. Create <NAS_DATA_DIR>/{deploy/src,pg-data,cardigann-defs} on the NAS
           (pg-data set no-CoW with `chattr +C` while still empty).
        2. rsync deploy/docker-compose.yml + docker-compose.nas.yml to
           <NAS_DATA_DIR>/deploy/src and apk/ to <NAS_DATA_DIR>/deploy/apk; write
           deploy/.env there = deploy/.env overlaid with deploy/.env.nas (mode 600).
        3. Render, ON the NAS, `deploy/compose.yaml` = base + overlay + .env fully
           interpolated (`docker compose config`). That single file is what
           Container Manager's Project imports and what `nas up` / `nas pull` use,
           so the NAS runs the stack on its own — no Mac in the loop.
        4. `--images`: also `docker save | gzip | ssh docker load` the locally
           built `:nas` images (offline/emergency path; normally the NAS pulls
           from GHCR — .github/workflows/images.yml).

        ## Notes
        - Idempotent; run after any compose/env change, then `nas up` (or
          Container Manager → project → Action → Build/Restart) to apply.
        - Requires passwordless ssh to the NAS and NAS_DOCKER working without a
          password (sudoers line, see deploy/README).
        """,
        risk_level="safe",
    ),
)
@angreal.argument(
    name="images", long="images", takes_value=False, is_flag=True,
    help="also ship the locally built :nas images (docker save | ssh | docker load)",
)
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="with --images: only this image (skadi | skadi-downloader-worker)",
)
def push(images=False, service=None):
    c = _cfg()
    d = shlex.quote(c["NAS_DATA_DIR"])
    print(f"==> preparing {c['NAS_DATA_DIR']} on {c['NAS_SSH_HOST']}")
    prep = (
        f"set -e; mkdir -p {d}/deploy/src {d}/pg-data {d}/cardigann-defs; "
        # no-CoW for postgres — only meaningful before initdb writes anything.
        f"if [ -z \"$(ls -A {d}/pg-data)\" ]; then chattr +C {d}/pg-data 2>/dev/null || true; fi; "
        # Leftovers from the pre-render layout (compose files directly in deploy/).
        f"rm -f {d}/deploy/docker-compose.yml {d}/deploy/docker-compose.nas.yml; "
        # Docker seeds an empty bind-backed volume from the image dir (uid 1000);
        # hand it to the uid the containers run as so the daemon can write defs.
        f"{c['NAS_DOCKER']} run --rm -v {d}/cardigann-defs:/d alpine:3.20 chown -R {c.get('NAS_PUID', '1000')}:{c.get('NAS_PGID', '100')} /d"
    )
    if _ssh(c, prep).returncode != 0:
        raise SystemExit("remote prepare failed (ssh/key/paths?)")

    print("==> syncing compose sources → deploy/src, apk/ → deploy/apk")
    host = f"{c['NAS_SSH_USER']}@{c['NAS_SSH_HOST']}"
    rsh = f"ssh -p {c['NAS_SSH_PORT']}"
    srcs = [os.path.join(deploy_dir, f) for f in NAS_COMPOSE_FILES]
    rc = subprocess.run(["rsync", "-rlt", "-e", rsh] + srcs
                        + [f"{host}:{c['NAS_DATA_DIR']}/deploy/src/"], cwd=cwd).returncode
    if rc != 0:
        raise SystemExit(rc)
    apk = os.path.join(deploy_dir, "apk")
    if os.path.isdir(apk):
        rc = subprocess.run(["rsync", "-rlt", "--delete-after", "-e", rsh, apk + "/",
                             f"{host}:{c['NAS_DATA_DIR']}/deploy/apk/"], cwd=cwd).returncode
        if rc != 0:
            raise SystemExit(rc)

    print("==> assembling .env (deploy/.env + deploy/.env.nas overrides)")
    merged = _read_env(NAS_BASE_ENV)
    merged.update(_read_env(NAS_NAS_ENV))
    with tempfile.NamedTemporaryFile("w", delete=False, prefix="skadi-nas-env.") as tf:
        tf.write("# assembled by `angreal nas push` — edit deploy/.env / .env.nas instead\n")
        for k, v in merged.items():
            tf.write(f"{k}={v}\n")
        tmp = tf.name
    # ssh + cat rather than scp: DSM has SFTP off by default and modern scp
    # speaks SFTP, which just says "Connection closed".
    try:
        with open(tmp, "rb") as f:
            rc = _ssh(c, f"umask 077; cat > {d}/deploy/.env && chmod 600 {d}/deploy/.env", stdin=f).returncode
    finally:
        os.unlink(tmp)
    if rc != 0:
        raise SystemExit(rc)

    print("==> rendering deploy/compose.yaml on the NAS (base + overlay + .env)")
    r = _ssh(c, _remote_render(c), capture_output=True, text=True)
    if r.returncode != 0:
        print(r.stderr)
        raise SystemExit("render failed — fix the compose/env and re-run push")
    print(f"    compose.yaml rendered ({r.stdout.strip()} top-level services/volumes/networks)")

    if not images:
        return 0
    targets = [service] if service else list(NAS_IMAGES)
    for img in targets:
        ref = f"{img}:{NAS_LOCAL_TAG}"
        print(f"==> shipping {ref} (docker save | gzip | ssh docker load)")
        pipeline = (
            f"docker save {shlex.quote(ref)} | gzip -1 | "
            + " ".join(shlex.quote(a) for a in _ssh_base(c))
            + f" {shlex.quote('gunzip | ' + c['NAS_DOCKER'] + ' load')}"
        )
        rc = subprocess.run(pipeline, shell=True, cwd=cwd).returncode
        if rc != 0:
            raise SystemExit(rc)
    return 0


@nas()
@angreal.command(
    name="pull",
    about="docker compose pull on the NAS (fetch the newest GHCR images), no restart",
)
def pull():
    c = _cfg()
    raise SystemExit(_ssh(c, _remote_compose(c, "pull")).returncode)


@nas()
@angreal.command(
    name="up",
    about="docker compose up -d on the NAS, then wait for skadi /health",
    tool=angreal.ToolDescription(
        """
        Bring the stack up on the NAS (compose project `skadi` there) from the
        files `nas push` shipped, and wait for http://<NAS>:<SKADI_PORT>/health.

        ## Notes
        - First start waits on gluetun's VPN healthcheck: 1–2 minutes is normal.
        - Needs /dev/net/tun on the NAS (boot task: insmod /lib/modules/tun.ko).
        """,
        risk_level="safe",
    ),
)
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="only (re)create this service",
)
@angreal.argument(
    name="recreate", long="recreate", takes_value=False, is_flag=True,
    help="--force-recreate (pick up a newly pushed image)",
)
def up(service=None, recreate=False):
    c = _cfg()
    args = "up -d" + (" --force-recreate" if recreate else "") + (f" {service}" if service else "")
    if _ssh(c, _remote_compose(c, args)).returncode != 0:
        raise SystemExit(1)
    url = _health_url(c)
    print(f"stack starting on the NAS; waiting for {url} ...")
    if not _NAS_wait_healthy(url):
        print("skadi did not become healthy — `angreal nas logs` / `angreal nas status`")
        raise SystemExit(1)
    print(f"healthy: {url.rsplit('/', 1)[0]}")
    return 0


@nas()
@angreal.command(name="down", about="stop & remove the NAS stack's containers (keeps data dirs)")
def down():
    c = _cfg()
    raise SystemExit(_ssh(c, _remote_compose(c, "down")).returncode)


@nas()
@angreal.command(name="status", about="container status on the NAS + skadi /health + memory")
def status():
    c = _cfg()
    _ssh(c, _remote_compose(c, "ps"))
    _ssh(c, f"{c['NAS_DOCKER']} stats --no-stream --format "
            "'table {{.Name}}\\t{{.MemUsage}}\\t{{.CPUPerc}}'; free -m | head -2")
    url = _health_url(c)
    try:
        with urllib.request.urlopen(url, timeout=3) as r:
            print(f"\nskadi {url}: HTTP {r.status}")
    except Exception as e:
        print(f"\nskadi {url}: unreachable ({e})")
    return 0


@nas()
@angreal.command(name="logs", about="tail a NAS service's logs (default skadi)")
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="service to tail (default: skadi)",
)
def logs(service=None):
    c = _cfg()
    raise SystemExit(_ssh(c, _remote_compose(c, f"logs -f --tail=100 {service or 'skadi'}")).returncode)


@nas()
@angreal.command(name="psql", about="run a SQL statement (or open psql) against the NAS database")
@angreal.argument(
    name="sql", long="command", short="c", takes_value=True,
    help="SQL to run non-interactively (psql -Atc)",
)
def psql(sql=None):
    c = _cfg()
    env = _read_env(NAS_BASE_ENV)
    user, db = env.get("POSTGRES_USER", "skadi"), env.get("POSTGRES_DB", "skadi")
    if sql:
        cmd = _remote_compose(c, f"exec -T postgres psql -U {user} -d {db} -Atc {shlex.quote(sql)}")
        raise SystemExit(_ssh(c, cmd).returncode)
    cmd = _remote_compose(c, f"exec postgres psql -U {user} -d {db}")
    raise SystemExit(subprocess.run(_ssh_base(c)[:-1] + ["-t", _ssh_base(c)[-1], cmd]).returncode)


@nas()
@angreal.command(
    name="db-migrate",
    about="pg_dump the Mac stack's database and restore it INTO the NAS database (cut-over)",
    tool=angreal.ToolDescription(
        """
        One-shot data move for the cut-over (SKADI-I-0056): `pg_dump -Fc` from
        the Mac's `skadi` postgres → `pg_restore --clean --if-exists` into the
        NAS's postgres. The NAS database is OVERWRITTEN with the Mac's contents.

        ## Preconditions (checked)
        - The Mac's skadi daemon + worker containers are STOPPED (no writes in
          flight): `docker stop skadi-skadi-1 skadi-skadi-downloader-worker-1`.
        - The NAS stack's postgres is up (`angreal nas up --service postgres`).

        ## Notes
        - The Mac's database is only read. The dump is kept at deploy/.nas-dump/
          for a manual re-run.
        - Stop the NAS's skadi + worker before running so they don't hold
          connections / see a half-restored schema; `nas up` afterwards.
        """,
        risk_level="destructive",
    ),
)
@angreal.argument(
    name="force", long="force", takes_value=False, is_flag=True,
    help="skip the 'Mac daemon/worker must be stopped' check",
)
def db_migrate(force=False):
    c = _cfg()
    env = _read_env(NAS_BASE_ENV)
    user, db = env.get("POSTGRES_USER", "skadi"), env.get("POSTGRES_DB", "skadi")
    running = subprocess.run(
        ["docker", "ps", "--format", "{{.Names}}",
         "--filter", "name=skadi-skadi-1", "--filter", "name=skadi-skadi-downloader-worker-1"],
        capture_output=True, text=True,
    ).stdout.split()
    running = [n for n in running if n in ("skadi-skadi-1", "skadi-skadi-downloader-worker-1")]
    if running and not force:
        raise SystemExit(f"refusing: Mac containers still running: {' '.join(running)} "
                         f"(stop them so no writes are in flight, or --force)")
    dump_dir = os.path.join(deploy_dir, ".nas-dump")
    os.makedirs(dump_dir, exist_ok=True)
    dump = os.path.join(dump_dir, f"skadi-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}.dump")
    print(f"==> pg_dump from the Mac stack → {dump}")
    with open(dump, "wb") as f:
        rc = subprocess.run(
            ["docker", "compose", "-f", os.path.join(deploy_dir, "docker-compose.yml"),
             "exec", "-T", "postgres", "pg_dump", "-U", user, "-Fc", db],
            cwd=deploy_dir, stdout=f,
        ).returncode
    if rc != 0 or os.path.getsize(dump) == 0:
        raise SystemExit("pg_dump failed")
    print(f"    {os.path.getsize(dump) / 1e6:.1f} MB")
    print("==> pg_restore into the NAS postgres (--clean --if-exists)")
    restore = _remote_compose(
        c, f"exec -T postgres pg_restore -U {user} -d {db} --clean --if-exists --no-owner --no-privileges"
    )
    with open(dump, "rb") as f:
        rc = _ssh(c, restore, stdin=f).returncode
    if rc != 0:
        print("pg_restore exited non-zero — it reports harmless 'does not exist' errors "
              "on a fresh DB; verify with `angreal nas psql -c 'select count(*) from downloads'`")
    return 0
