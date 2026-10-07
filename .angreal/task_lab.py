import angreal
import os
import subprocess
import sys
import time
import urllib.request

cwd = os.path.join(angreal.get_root(), '..')
deploy_dir = os.path.join(cwd, "deploy")

lab = angreal.command_group(
    name="lab",
    about="run an ISOLATED second copy of the deploy stack (project skadi-lab) for experiments",
)

# The lab is the production compose file plus an overlay (deploy/docker-compose.lab.yml)
# under its OWN project name, env file, subnet, ports, image tags and local storage
# dir — see the overlay header (SKADI-T-0393). Everything here is pinned to project
# `skadi-lab`; nothing in this module can address the live `skadi` project.
LAB_PROJECT = "skadi-lab"
LAB_COMPOSE_FILES = [
    os.path.join(deploy_dir, "docker-compose.yml"),
    os.path.join(deploy_dir, "docker-compose.lab.yml"),
]
LAB_ENV_FILE = os.path.join(deploy_dir, ".env.lab")
LAB_STORAGE_DIR = os.path.join(deploy_dir, ".lab", "storage")
# Readiness, not liveness — see SKADI-T-0475: bare `/health` is the SPA fallback
# and answers 200 as soon as the socket binds.
LAB_HEALTH_URL = "http://127.0.0.1:8091/api/v1/health/ready"


def _LAB_compose(args):
    # SKADI-T-0533: every module-level name in `.angreal/task_*.py` lives in ONE
    # shared namespace, so this helper used to be shadowed by the deploy task's
    # `_compose` (and `PROJECT` by the NAS task's "skadi"). `angreal lab up` then
    # ran against the PRODUCTION project — it started the live stack and left its
    # daemon crash-looping. The names here are prefixed so they cannot be
    # shadowed, and this assertion means a future collision fails loudly instead
    # of quietly addressing prod.
    assert LAB_PROJECT == "skadi-lab", (
        f"lab tasks are pinned to project 'skadi-lab', got {LAB_PROJECT!r} — "
        "a module-level name collision (SKADI-T-0533)"
    )
    assert LAB_COMPOSE_FILES[-1].endswith("docker-compose.lab.yml"), (
        f"lab tasks must apply the lab overlay, got {LAB_COMPOSE_FILES!r} — "
        "a module-level name collision (SKADI-T-0533)"
    )
    cmd = ["docker", "compose", "-p", LAB_PROJECT, "--env-file", LAB_ENV_FILE]
    for f in LAB_COMPOSE_FILES:
        cmd += ["-f", f]
    # cwd=deploy so the base file's relative `./apk` mount resolves like prod.
    return subprocess.run(cmd + args, cwd=deploy_dir, env=_LAB_build_env())


def _LAB_build_env():
    """The environment for compose, with SKADI_BUILD_COMMIT set (SKADI-T-0684).

    `.git/` is not in the image's build context, so the Dockerfile takes the
    commit as a build arg, which the compose file fills from this variable. A
    value already in the environment wins; outside a git checkout it stays
    unset, and the daemon reports `unknown`.
    """
    env = dict(os.environ)
    if not env.get("SKADI_BUILD_COMMIT"):
        r = subprocess.run(
            ["git", "rev-parse", "--short=12", "HEAD"],
            cwd=cwd, capture_output=True, text=True,
        )
        if r.returncode == 0 and r.stdout.strip():
            env["SKADI_BUILD_COMMIT"] = r.stdout.strip()
    return env


def _LAB_ensure_storage():
    # The overlay bind-mounts deploy/.lab/storage at /mnt/storage; pre-create the
    # library root + download dirs so the containers don't race to mkdir as root.
    for sub in ("skadi/downloads/complete", "skadi/downloads/incomplete"):
        os.makedirs(os.path.join(LAB_STORAGE_DIR, sub), exist_ok=True)


def _LAB_wait_healthy(timeout_s=120):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(LAB_HEALTH_URL, timeout=3) as r:
                if r.status == 200:
                    return True
        except Exception:
            pass
        time.sleep(3)
    return False


LAB_IMAGES = ("skadi", "skadi-downloader-worker")


def _LAB_image_exists(ref):
    return (
        subprocess.run(
            ["docker", "image", "inspect", ref],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        ).returncode
        == 0
    )


def _LAB_missing_images():
    """Which `:lab` images the compose overlay expects but cannot find."""
    return [img for img in LAB_IMAGES if not _LAB_image_exists(f"{img}:lab")]


@lab()
@angreal.command(
    name="up",
    about="bring the lab stack up (postgres + skadi + worker, NO vpn) and wait for health",
    tool=angreal.ToolDescription(
        """
        Start the isolated lab stack (compose project `skadi-lab`): postgres, the
        skadi daemon on http://127.0.0.1:8091 and the downloader worker, with
        local storage under deploy/.lab/storage and NO VPN containers.

        ## When to use
        - To experiment (worker memory profiling, upgrade rehearsals, destructive
          tests) without touching the production `skadi` project.

        ## Notes
        - Builds the `:lab` images if missing; `--build` forces a rebuild;
          `--from-prod` tags the current prod `:latest` images as `:lab` instead
          of building (fast way to get a lab copy of what is running).
        - The worker has NO kill switch here — local/synthetic torrents only.
        """,
        risk_level="safe",
    ),
)
@angreal.argument(
    name="build", long="build", takes_value=False, is_flag=True,
    help="rebuild the :lab images before starting",
)
@angreal.argument(
    name="from_prod", long="from-prod", takes_value=False, is_flag=True,
    help="tag prod's skadi:latest / skadi-downloader-worker:latest as :lab instead of building",
)
def up(build=False, from_prod=False):
    _LAB_ensure_storage()
    if from_prod:
        for img in LAB_IMAGES:
            src = f"{img}:latest"
            if not _LAB_image_exists(src):
                # Say which image and how to get it, rather than letting `docker
                # tag` fail with a bare "No such image" (SKADI-T-0490).
                print(
                    f"--from-prod needs {src}, which does not exist.\n"
                    f"Build it first (`angreal deploy build --service {img}`), or run "
                    f"`angreal lab up --build` to build the lab images directly.",
                    file=sys.stderr,
                )
                raise SystemExit(1)
            if subprocess.run(["docker", "tag", src, f"{img}:lab"]).returncode != 0:
                raise SystemExit(1)

    # SKADI-T-0490: without this, a missing `:lab` image makes compose silently
    # start a full workspace build — ten-plus minutes of Rust compilation with no
    # indication that is what is happening, which is exactly what it looked like
    # during the SKADI-T-0533 investigation. Say so, and make the caller choose.
    if not build:
        missing = _LAB_missing_images()
        if missing:
            print(
                "missing lab image(s): " + ", ".join(f"{m}:lab" for m in missing) + "\n"
                "compose would build them from source now (several minutes). Either:\n"
                "  angreal lab up --build       # build them, deliberately\n"
                "  angreal lab up --from-prod   # retag the existing :latest images",
                file=sys.stderr,
            )
            raise SystemExit(1)

    args = ["up", "-d"]
    if build:
        args.append("--build")
    if _LAB_compose(args).returncode != 0:
        raise SystemExit(1)
    print("lab starting; waiting for skadi readiness ...")
    if not _LAB_wait_healthy():
        print("lab skadi did not become healthy — check `angreal lab logs`")
        raise SystemExit(1)
    print(f"lab healthy: {LAB_HEALTH_URL.rsplit('/', 1)[0]}  (token: see deploy/.env.lab)")
    return 0


@lab()
@angreal.command(name="down", about="stop & remove the lab containers (keeps lab volumes)")
def down():
    raise SystemExit(_LAB_compose(["down"]).returncode)


@lab()
@angreal.command(
    name="reset",
    about="stop the lab AND wipe its volumes + local storage dir (project skadi-lab only)",
    tool=angreal.ToolDescription(
        """
        Tear the lab stack down with `down -v` (lab postgres data, cardigann defs)
        and delete deploy/.lab/storage. This is the ONLY place a volume wipe is
        allowed, and it is hard-pinned to compose project `skadi-lab` — it cannot
        reach the production `skadi` project's volumes.

        ## When to use
        - To start an experiment from a clean database / empty library.
        """,
        risk_level="destructive",
    ),
)
def reset():
    assert LAB_PROJECT == "skadi-lab", "reset is pinned to the lab project"
    rc = _LAB_compose(["down", "-v", "--remove-orphans"]).returncode
    if rc != 0:
        raise SystemExit(rc)
    if os.path.isdir(LAB_STORAGE_DIR):
        subprocess.run(["rm", "-rf", LAB_STORAGE_DIR], check=False)
        print(f"removed {LAB_STORAGE_DIR}")
    return 0


@lab()
@angreal.command(name="build", about="build a lab image (default skadi; --service skadi-downloader-worker)")
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="service to build (default: skadi)",
)
def build(service=None):
    raise SystemExit(_LAB_compose(["build", service or "skadi"]).returncode)


@lab()
@angreal.command(
    name="redeploy",
    about="rebuild a lab service (default skadi), recreate its container, prune builder cache",
)
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="service to rebuild + recreate (default: skadi)",
)
def redeploy(service=None):
    svc = service or "skadi"
    _LAB_ensure_storage()
    if _LAB_compose(["build", svc]).returncode != 0:
        raise SystemExit(1)
    if _LAB_compose(["up", "-d", "--force-recreate", svc]).returncode != 0:
        raise SystemExit(1)
    # Same discipline as `angreal deploy redeploy`: builds fill the Docker VM disk.
    subprocess.run(["docker", "builder", "prune", "-af"], cwd=cwd)
    if svc == "skadi":
        print("recreated; waiting for lab skadi /health ...")
        if not _LAB_wait_healthy():
            print("lab skadi did not become healthy — check `angreal lab logs`")
            raise SystemExit(1)
        print("lab skadi healthy: http://127.0.0.1:8091")
    return 0


@lab()
@angreal.command(name="logs", about="tail a lab service's logs (default skadi)")
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="service to tail (default: skadi)",
)
def logs(service=None):
    raise SystemExit(_LAB_compose(["logs", "-f", "--tail=100", service or "skadi"]).returncode)


@lab()
@angreal.command(name="status", about="show lab container status + skadi /health")
def status():
    _LAB_compose(["ps"])
    try:
        with urllib.request.urlopen(LAB_HEALTH_URL, timeout=3) as r:
            print(f"\nlab skadi /health: HTTP {r.status}")
    except Exception as e:
        print(f"\nlab skadi /health: unreachable ({e})")
    return 0


@lab()
@angreal.command(name="psql", about="run a SQL statement (or open psql) against the LAB database")
@angreal.argument(
    name="sql", long="sql", short="c", takes_value=True,
    help="statement to run non-interactively (omit for an interactive psql)",
)
def psql(sql=None):
    args = ["exec"]
    if sql:
        args += ["-T", "postgres", "psql", "-U", "skadi", "-d", "skadi", "-Atc", sql]
    else:
        args += ["postgres", "psql", "-U", "skadi", "-d", "skadi"]
    raise SystemExit(_LAB_compose(args).returncode)
