import angreal
import os
import subprocess
import sys
import time
import urllib.request

cwd = os.path.join(angreal.get_root(), '..')

deploy = angreal.command_group(
    name="deploy",
    about="build & run the production docker-compose stack (deploy/docker-compose.yml)",
)

# The live stack is compose project `skadi` (pinned via `name: skadi` in the file),
# so every command here targets the same containers regardless of the working dir.
# The UI/API is served on :8090 (NOT :8080 — that's the in-container port).
#
# SAFETY: never `down -v` on this stack — the postgres volume holds the REAL
# library database (movies/TV/audiobooks + config + secrets). Only the throwaway
# `angreal db` test database is safe to wipe.
DEP_DEPLOY_DIR = os.path.join(cwd, "deploy")
DEP_COMPOSE_FILE = os.path.join(DEP_DEPLOY_DIR, "docker-compose.yml")
# Secrets from files (SKADI-T-0702): opt-in. The overlay is applied only when
# deploy/secrets/ exists, so a deploy that keeps its secrets in deploy/.env runs
# exactly the compose model it ran before.
DEP_SECRETS_OVERLAY = os.path.join(DEP_DEPLOY_DIR, "docker-compose.secrets.yml")
DEP_SECRETS_DIR = os.path.join(DEP_DEPLOY_DIR, "secrets")
# The READINESS probe, not `/health` (SKADI-T-0475). Bare `/health` is served by
# the SPA fallback, so it answers 200 the instant the listener binds — before
# migrations have run or any provider exists — and this gate reported a healthy
# stack immediately. `/api/v1/health/ready` checks the database and that the
# supervisor has published providers.
DEP_HEALTH_URL = "http://127.0.0.1:8090/api/v1/health/ready"


def _DEP_subnet():
    """The subnet the stack's network claims, from `deploy/.env` or the default.

    Mirrors the compose default in `docker-compose.yml` (`${COMPOSE_SUBNET:-…}`);
    if those two ever disagree the preflight would inspect the wrong range and
    pass while the real one is held.
    """
    env_file = os.path.join(DEP_DEPLOY_DIR, ".env")
    try:
        with open(env_file) as fh:
            for line in fh:
                line = line.strip()
                if line.startswith("COMPOSE_SUBNET="):
                    value = line.split("=", 1)[1].strip().strip('"').strip("'")
                    if value:
                        return value
    except OSError:
        pass
    return "172.28.0.0/16"


def _DEP_ghost_network():
    """A network holding our subnet that is NOT this project's (SKADI-T-0478).

    On 2026-09-06 a teardown left `skadi_default` behind holding 172.28.0.0/16,
    and every `compose up` for twenty minutes failed with "Pool overlaps with
    other one on this address space" — a message that names neither the subnet
    nor the network squatting on it, so the recovery is only obvious once you
    already know it.

    Returns the offending network's name, or None. Best-effort: a docker that
    cannot be inspected is not a reason to refuse to start, since `up` is about
    to give a real error anyway.
    """
    subnet = _DEP_subnet()
    try:
        names = subprocess.run(
            ["docker", "network", "ls", "--format", "{{.Name}}"],
            capture_output=True, text=True, timeout=15,
        )
        if names.returncode != 0:
            return None
        for name in names.stdout.split():
            # `skadi_default` is the ghost this guards against; the live
            # project's own networks are `skadi_*` too, but compose reuses
            # those rather than colliding with them. Only report a network
            # that holds the subnet and is not currently attached to a
            # running container of ours.
            got = subprocess.run(
                ["docker", "network", "inspect", name,
                 "--format", "{{range .IPAM.Config}}{{.Subnet}} {{end}}{{len .Containers}}"],
                capture_output=True, text=True, timeout=15,
            )
            if got.returncode != 0:
                continue
            parts = got.stdout.split()
            if not parts:
                continue
            subnets, attached = parts[:-1], parts[-1]
            if subnet in subnets and attached == "0":
                return name
    except (OSError, subprocess.SubprocessError):
        return None
    return None


def _DEP_preflight():
    """Warn about a ghost network before `up` fails on it (SKADI-T-0478)."""
    ghost = _DEP_ghost_network()
    if not ghost:
        return
    subnet = _DEP_subnet()
    print(
        f"preflight: network '{ghost}' already holds {subnet} and has no "
        f"containers attached.\n"
        f"  `up` will fail with \"Pool overlaps with other one on this address "
        f"space\".\n"
        f"  Recovery:\n"
        f"    docker network inspect {ghost}\n"
        f"    docker network rm {ghost}\n"
        f"    angreal deploy up\n",
        file=sys.stderr,
    )


def _DEP_compose(args):
    # SKADI-T-0477: be explicit about the env file and the project directory
    # rather than relying on compose's implicit resolution. Run from the repo
    # root, compose looks for `.env` beside the *compose file* only when
    # --project-directory says so; without it the deploy stack came up with
    # STORAGE_NFS_ADDR/PATH unset (warned, not failed), which silently maps the
    # NFS volume to an empty address. `angreal lab` already
    # passes --env-file; this brings prod in line.
    #
    # --project-directory also anchors the base file's relative `./apk` mount, so
    # it resolves the same whichever directory the command is invoked from.
    return subprocess.run(_DEP_compose_argv(args), cwd=DEP_DEPLOY_DIR, env=_DEP_build_env())


def _DEP_build_env():
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


def _DEP_compose_argv(args):
    """The compose command line `_DEP_compose` runs, for callers that need to
    capture output (sidecar presence checks) rather than stream it."""
    env_file = os.path.join(DEP_DEPLOY_DIR, ".env")
    cmd = ["docker", "compose", "--project-directory", DEP_DEPLOY_DIR]
    if os.path.exists(env_file):
        cmd += ["--env-file", env_file]
    cmd += ["-f", DEP_COMPOSE_FILE]
    if os.path.isdir(DEP_SECRETS_DIR):
        cmd += ["-f", DEP_SECRETS_OVERLAY]
    return cmd + list(args)


def _DEP_wait_healthy(timeout_s=180):
    """Block until the skadi daemon reports READY (or time out).

    First boot waits on gluetun's VPN healthcheck, so give it generous time."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(DEP_HEALTH_URL, timeout=3) as r:
                if r.status == 200:
                    return True
        except Exception:
            pass
        time.sleep(3)
    return False


# First run (SKADI-T-0703): the secrets `angreal deploy init` generates.
# (variable in .env, file under deploy/secrets/, bytes of randomness → hex)
DEP_INIT_SECRETS = [
    ("POSTGRES_PASSWORD", "postgres_password", 16),
    ("SKADI_API_TOKEN", "skadi_api_token", 24),
    ("SKADI_SECRET_KEY", "skadi_secret_key", 32),
]
# Written empty in --secrets mode: the overlay mounts it, and tailscale treats
# an empty file as "no key" (the profile is off unless COMPOSE_PROFILES says so).
DEP_INIT_EMPTY_FILES = ["tailscale_authkey"]


def _DEP_render_env(example, values):
    """`example` (the text of .env.example) with each `KEY=` line of a key in
    `values` set to its value. A commented `#KEY=…` line is uncommented when
    no plain line for that key exists. Every other line is kept as it is."""
    plain = set()
    for line in example.splitlines():
        if "=" in line and not line.startswith("#"):
            plain.add(line.split("=", 1)[0].strip())
    out = []
    done = set()
    for line in example.splitlines(keepends=True):
        stripped = line.rstrip("\r\n")
        key = None
        if "=" in stripped:
            head = stripped.split("=", 1)[0]
            if not head.startswith("#"):
                key = head.strip()
            elif head[1:].strip() not in plain and " " not in head[1:].strip():
                key = head[1:].strip()
        if key in values and key not in done:
            out.append(f"{key}={values[key]}\n")
            done.add(key)
        else:
            out.append(line)
    return "".join(out)


def _DEP_write_new(path, text, mode):
    """Create `path` with `text`; fail if it exists (O_EXCL, so a race cannot
    overwrite a file that appeared after the check)."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(fd, "w") as fh:
        fh.write(text)
    os.chmod(path, mode)


@deploy()
@angreal.command(
    name="init",
    about="write deploy/.env (or deploy/secrets/*) with generated secrets; never overwrites",
    tool=angreal.ToolDescription(
        """
        First run of a new install: write deploy/.env from deploy/.env.example
        with POSTGRES_PASSWORD, SKADI_API_TOKEN and SKADI_SECRET_KEY generated.
        With --secrets, the three values go to files under deploy/secrets/
        instead (the secrets-from-files overlay) and stay empty in .env.

        ## When to use
        - Once, on a new host, before the first `angreal deploy up`.

        ## Notes
        - Refuses to run when deploy/.env exists, or when a secret file that it
          would write exists. It never overwrites a secret: a changed
          POSTGRES_PASSWORD locks the stack out of its database.
        - After it, edit the VPN and storage values in .env, then run
          `angreal deploy up`. The daemon registers the built-in downloader,
          creates the default profiles and enables the domains on its first boot.
        - --no-default-indexers sets SKADI_DEFAULT_INDEXERS=false: the daemon
          then does not register the curated public trackers.
        - --deploy-dir writes into another directory (a scratch copy for tests).
        """,
        risk_level="safe",
    ),
)
@angreal.argument(
    name="secrets", long="secrets", takes_value=False, is_flag=True,
    help="write the secrets to deploy/secrets/* files, not to .env",
)
@angreal.argument(
    name="no_default_indexers", long="no-default-indexers", takes_value=False, is_flag=True,
    help="set SKADI_DEFAULT_INDEXERS=false: do not register the curated public trackers",
)
@angreal.argument(
    name="deploy_dir", long="deploy-dir", takes_value=True,
    help="the directory to write into (default: deploy/ of this checkout)",
)
def init(secrets=False, no_default_indexers=False, deploy_dir=None):
    import secrets as _secrets

    target = os.path.abspath(deploy_dir) if deploy_dir else DEP_DEPLOY_DIR
    env_path = os.path.join(target, ".env")
    secrets_dir = os.path.join(target, "secrets")
    example_path = os.path.join(target, ".env.example")
    if not os.path.exists(example_path):
        example_path = os.path.join(DEP_DEPLOY_DIR, ".env.example")

    if os.path.exists(env_path):
        print(
            f"{env_path} exists. init does not overwrite it: new secrets would\n"
            "lock the stack out of its database. Edit the file, or move it away\n"
            "first if you really want a new one.",
            file=sys.stderr, flush=True,
        )
        raise SystemExit(1)
    if secrets:
        names = [f for _, f, _ in DEP_INIT_SECRETS] + DEP_INIT_EMPTY_FILES
        there = [n for n in names if os.path.exists(os.path.join(secrets_dir, n))]
        if there:
            print(
                f"{secrets_dir} already holds: {', '.join(there)}\n"
                "init does not overwrite a secret file.",
                file=sys.stderr, flush=True,
            )
            raise SystemExit(1)
    elif os.path.isdir(secrets_dir):
        print(
            f"{secrets_dir} exists, so `angreal deploy` applies the secrets overlay\n"
            "and would ignore secrets written to .env. Run `angreal deploy init\n"
            "--secrets`, or move that directory away.",
            file=sys.stderr, flush=True,
        )
        raise SystemExit(1)

    with open(example_path) as fh:
        example = fh.read()
    generated = {var: _secrets.token_hex(n) for var, _, n in DEP_INIT_SECRETS}
    values = {}
    if secrets:
        os.makedirs(secrets_dir, mode=0o700, exist_ok=True)
        os.chmod(secrets_dir, 0o700)
        for var, name, _ in DEP_INIT_SECRETS:
            # 0444: the containers run as other uids (postgres 999, skadi PUID);
            # the 0700 directory keeps other host users out.
            _DEP_write_new(os.path.join(secrets_dir, name), generated[var] + "\n", 0o444)
            values[var] = ""
        for name in DEP_INIT_EMPTY_FILES:
            _DEP_write_new(os.path.join(secrets_dir, name), "", 0o444)
    else:
        values.update(generated)
    if no_default_indexers:
        values["SKADI_DEFAULT_INDEXERS"] = "false"
    _DEP_write_new(env_path, _DEP_render_env(example, values), 0o600)

    where = secrets_dir + "/" if secrets else env_path
    print(f"wrote {env_path}", flush=True)
    print(f"generated POSTGRES_PASSWORD, SKADI_API_TOKEN and SKADI_SECRET_KEY in {where}")
    print(
        "\nNext:\n"
        f"  1. Edit {env_path}: the VPN values (VPN_SERVICE_PROVIDER, OPENVPN_USER /\n"
        "     OPENVPN_PASSWORD or the WireGuard values) and the storage values\n"
        "     (STORAGE_NFS_ADDR, STORAGE_NFS_PATH, SKADI_LIBRARY_ROOT).\n"
        "  2. angreal deploy up\n"
        "Keep a copy of the secrets in your password manager.",
        flush=True,
    )
    return 0


@deploy()
@angreal.command(
    name="up",
    about="bring the whole stack up (detached), then wait for skadi to be healthy",
    tool=angreal.ToolDescription(
        """
        Start the full production stack (skadi, postgres, gluetun VPN, downloader
        worker, flaresolverr, watchdogs) detached, then wait for the daemon's
        /api/v1/health/ready to report ready.

        ## When to use
        - After `angreal deploy down`, or on a fresh machine, to run skadi locally.

        ## Notes
        - Serves the UI/API on http://127.0.0.1:8090.
        - The worker gates on gluetun's VPN healthcheck, so the FIRST start can
          take a minute or two — that's expected, not a hang. Do NOT run this
          under a short foreground timeout; it is detached and self-waits.
        - Builds any missing images; use `--build` to force a rebuild first.
        """,
        risk_level="safe",
    ),
)
@angreal.argument(
    name="build", long="build", takes_value=False,
    help="rebuild images before starting",
)
def up(build=False):
    # Named before the failure rather than after it (SKADI-T-0478): the compose
    # error names neither the subnet nor the squatting network.
    _DEP_preflight()
    args = ["up", "-d"]
    if build:
        args.append("--build")
    if _DEP_compose(args).returncode != 0:
        raise SystemExit(1)
    print("stack starting; waiting for skadi readiness ...")
    if not _DEP_wait_healthy():
        print("skadi did not become healthy in time — check `angreal deploy logs`")
        raise SystemExit(1)
    print("skadi healthy: http://127.0.0.1:8090")
    return 0


@deploy()
@angreal.command(
    name="down",
    about="stop & remove the stack containers (KEEPS volumes — the library DB is safe)",
)
def down():
    # Deliberately NO `-v`: the postgres volume holds the real library database.
    raise SystemExit(_DEP_compose(["down"]).returncode)


@deploy()
@angreal.command(name="build", about="build the skadi image (or another service with --service)")
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="service to build (default: skadi)",
)
def build(service=None):
    raise SystemExit(_DEP_compose(["build", service or "skadi"]).returncode)


# Sidecars that share another service's network namespace (`network_mode:
# service:<x>`). Docker binds that namespace when the sidecar is *created*, so
# recreating the parent orphans them: after a `redeploy -s skadi` the tailscale
# node showed offline (the phone saw "Skadi offline") while the LAN address
# worked, and a gluetun recreate has done the same to flaresolverr. They must be
# force-recreated right after their parent. Profiled services are skipped when
# their profile is off (no container to recreate).
_DEP_NETNS_SIDECARS = {
    "skadi": [("tailscale", "tailscale")],
    "gluetun": [("skadi-downloader-worker", None), ("flaresolverr", None)],
}


def _DEP_reattach_sidecars(parent):
    for name, profile in _DEP_NETNS_SIDECARS.get(parent, []):
        pre = ["--profile", profile] if profile else []
        ps = subprocess.run(
            _DEP_compose_argv(pre + ["ps", "-q", name]),
            cwd=DEP_DEPLOY_DIR, capture_output=True, text=True,
        )
        if not ps.stdout.strip():
            continue
        print(f"re-attaching {name} to the new {parent} network namespace ...")
        if _DEP_compose(pre + ["up", "-d", "--force-recreate", "--no-deps", name]).returncode != 0:
            print(f"warning: could not recreate {name}; run it by hand")



@deploy()
@angreal.command(
    name="redeploy",
    about="rebuild a service (default skadi), recreate its container, prune builder cache",
    tool=angreal.ToolDescription(
        """
        The deploy-a-code-change flow: rebuild the service image, force-recreate
        its container off the new image, prune the builder cache, then wait for
        skadi health.

        ## When to use
        - After changing Rust/web/server code, to roll it onto the running stack.

        ## Notes
        - Defaults to `skadi`; `--service skadi-downloader-worker` targets the worker.
        - The builder prune is REQUIRED discipline: repeated image builds fill the
          Docker VM disk, and a full VM disk crashes postgres (learned the hard way).
        """,
        risk_level="safe",
    ),
)
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="service to rebuild + recreate (default: skadi)",
)
def redeploy(service=None):
    svc = service or "skadi"
    if _DEP_compose(["build", svc]).returncode != 0:
        raise SystemExit(1)
    if _DEP_compose(["up", "-d", "--force-recreate", svc]).returncode != 0:
        raise SystemExit(1)
    _DEP_reattach_sidecars(svc)
    # Reclaim builder cache — repeated builds fill the Docker VM disk.
    subprocess.run(["docker", "builder", "prune", "-af"], cwd=cwd)
    if svc == "skadi":
        print("recreated; waiting for skadi readiness ...")
        if not _DEP_wait_healthy():
            print("skadi did not become healthy — check `angreal deploy logs`")
            raise SystemExit(1)
        print("skadi healthy: http://127.0.0.1:8090")
    return 0


@deploy()
@angreal.command(name="logs", about="tail a service's logs (default skadi)")
@angreal.argument(
    name="service", long="service", short="s", takes_value=True,
    help="service to tail (default: skadi)",
)
def logs(service=None):
    raise SystemExit(_DEP_compose(["logs", "-f", "--tail=100", service or "skadi"]).returncode)


@deploy()
@angreal.command(
    name="pull",
    about="pull the published images from GHCR (needs SKADI_IMAGE_PREFIX set)",
    tool=angreal.ToolDescription(
        """
        Fetch the container images CI publishes, instead of building locally.

        ## Notes
        - Needs `SKADI_IMAGE_PREFIX=ghcr.io/skadi-media/` in `deploy/.env`.
          Without it the compose file names local images and there is nothing
          to pull — the task says so rather than silently doing nothing.
        - Set `SKADI_IMAGE_TAG` to a release (`0.1.4`) too: unset it is
          `local`, the tag of a local build, which no registry has.
        - Follow with `angreal deploy up` (or `redeploy`) to actually roll it
          out. Pulling on its own changes nothing that is running.
        """,
        risk_level="safe",
    ),
)
def pull():
    env_file = os.path.join(cwd, "deploy", ".env")
    prefix = ""
    tag = ""
    if os.path.exists(env_file):
        for line in open(env_file):
            if line.startswith("SKADI_IMAGE_PREFIX="):
                prefix = line.split("=", 1)[1].strip()
            elif line.startswith("SKADI_IMAGE_TAG="):
                tag = line.split("=", 1)[1].strip()
    if not prefix:
        print(
            "SKADI_IMAGE_PREFIX is not set in deploy/.env, so the compose file\n"
            "names local images and there is nothing to pull.\n\n"
            "  SKADI_IMAGE_PREFIX=ghcr.io/skadi-media/\n\n"
            "Both packages are public, so no `docker login` is needed.",
            flush=True,
        )
        # `raise SystemExit` rather than `return 1`, matching the other tasks:
        # returning a non-zero code exits by a path that does not flush stdout,
        # so the explanation above was written and then swallowed — the task
        # just failed silently, which is the opposite of the point.
        raise SystemExit(1)
    if not tag:
        # Unset, the compose default is `local` (SKADI-T-0701): the name of a
        # local build, which the registry does not have. Say so rather than
        # letting the pull fail with "manifest unknown".
        print(
            "SKADI_IMAGE_TAG is not set in deploy/.env. With a registry prefix,\n"
            "pin a published release, e.g.\n\n"
            "  SKADI_IMAGE_TAG=0.1.4\n",
            flush=True,
        )
        raise SystemExit(1)
    print(f"pulling {prefix}skadi:{tag} and {prefix}skadi-downloader-worker:{tag} …", flush=True)
    raise SystemExit(_DEP_compose(["pull", "skadi", "skadi-downloader-worker"]).returncode)


@deploy()
@angreal.command(name="status", about="show stack container status + skadi readiness")
def status():
    _DEP_compose(["ps"])
    try:
        with urllib.request.urlopen(DEP_HEALTH_URL, timeout=3) as r:
            print(f"\nskadi readiness: HTTP {r.status}")
    except Exception as e:
        print(f"\nskadi readiness: unreachable ({e})")
    return 0


@deploy()
@angreal.command(
    name="publish-apk",
    about="build + publish the Android APK to deploy/apk (served at /app/*)",
)
@angreal.argument(
    name="host", long="host", takes_value=False,
    help="use the host Android SDK/JDK instead of the docker builder",
)
def publish_apk(host=False):
    args = ["sh", os.path.join(cwd, "deploy", "publish-apk.sh")]
    if host:
        args.append("--host")
    raise SystemExit(subprocess.run(args, cwd=cwd).returncode)
