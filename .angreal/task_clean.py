"""Disk housekeeping (SKADI-T-0553).

Two artifact stores grow without bound on a dev box and neither is ever pruned
by anything else: cargo's `target/` (288 GB when this was written, against 80 GB
free) and Docker's image/build cache. Left alone they take the disk down, and the
first symptom is not "disk full" — it is builds and test runs being SIGKILLed,
which reads as a flaky test suite.
"""

import os
import subprocess
import sys

import angreal

cwd = os.path.join(angreal.get_root(), '..')
clean = angreal.command_group(name="clean", about="reclaim disk from build artifacts")


def _notify(msg):
    print(msg, file=sys.stderr, flush=True)


def _flag(name):
    """Whether `--name` was passed.

    Read from `sys.argv` rather than the decorated parameter because angreal
    2.x's `takes_value=False` arguments arrive as `None` whether the flag was
    given or not — the value never reaches the function. (The same bug makes
    `angreal test functional --ignored` and `angreal test all --ignored` silent
    no-ops today; see SKADI-T-0553.)
    """
    return f"--{name}" in sys.argv


def _du(path):
    """Human-readable size of `path`, or None if it isn't there."""
    if not os.path.exists(path):
        return None
    out = subprocess.run(["du", "-sh", path], capture_output=True, text=True)
    return out.stdout.split("\t")[0].strip() if out.returncode == 0 else None


def _free():
    out = subprocess.run(["df", "-h", "/"], capture_output=True, text=True)
    return out.stdout.strip().split("\n")[-1].split()[3] if out.returncode == 0 else "?"


@clean()
@angreal.command(name="cargo", about="remove cargo build artifacts (target/ dirs)")
@angreal.argument(
    name="yes", long="yes", takes_value=False,
    help="skip the confirmation prompt",
)
def clean_cargo(yes=False):
    """`cargo clean` for the workspace **and** the out-of-workspace web crate.

    `skadi-web` is excluded from the workspace and keeps its own `target/`, so a
    plain `cargo clean` at the root leaves several GB behind — the same
    exclusion that let its tests rot uncompiled (SKADI-T-0551).

    The cost is a full rebuild afterwards, which is why this asks first.
    """
    targets = [
        ("workspace", cwd),
        ("skadi-web", os.path.join(cwd, "crates", "skadi-web")),
    ]
    total = []
    for name, path in targets:
        size = _du(os.path.join(path, "target"))
        if size:
            total.append(f"  {name:<12} {size}")
    if not total:
        _notify("nothing to clean")
        raise SystemExit(0)

    _notify("will remove:")
    for line in total:
        _notify(line)
    _notify(f"free now: {_free()}")
    if not _flag("yes"):
        _notify("\nThis forces a full rebuild. Re-run with --yes to proceed.")
        raise SystemExit(1)

    for name, path in targets:
        if os.path.isdir(os.path.join(path, "target")):
            _notify(f"cleaning {name}...")
            subprocess.run(["cargo", "clean"], cwd=path)
    _notify(f"done. free now: {_free()}")
    raise SystemExit(0)


@clean()
@angreal.command(
    name="docker",
    about="reclaim Docker disk (build cache + dangling images by default)",
)
@angreal.argument(
    name="all", long="all", takes_value=False,
    help="also remove images not used by ANY container — including ones a "
         "running stack may want on its next pull or rollback",
)
@angreal.argument(
    name="yes", long="yes", takes_value=False,
    help="skip the confirmation prompt",
)
def clean_docker(all=False, yes=False):
    """Prune Docker artifacts, conservatively by default.

    The default touches only the build cache and **dangling** images — layers no
    tag points at. That is the large, safe win and it cannot affect a running
    stack.

    `--all` additionally removes images no container currently uses. On this
    machine that is not obviously safe: prod runs here, and an image that is
    merely stopped is exactly what a rollback needs. Hence a separate flag and a
    separate confirmation.
    """
    df = subprocess.run(["docker", "system", "df"], capture_output=True, text=True)
    if df.returncode != 0:
        _notify("docker is not reachable")
        raise SystemExit(1)
    _notify(df.stdout.strip())
    prune_all = _flag("all")

    if not _flag("yes"):
        scope = ("build cache + dangling images, PLUS every image no container "
                 "is using" if prune_all else "build cache + dangling images only")
        _notify(f"\nwould prune: {scope}")
        if prune_all:
            _notify("  --all can remove an image a stopped container or a "
                    "rollback still needs.")
        _notify("Re-run with --yes to proceed.")
        raise SystemExit(1)

    _notify("pruning build cache...")
    subprocess.run(["docker", "builder", "prune", "-f"], cwd=cwd)
    _notify("pruning images...")
    cmd = ["docker", "image", "prune", "-f"]
    if prune_all:
        cmd.append("-a")
    subprocess.run(cmd, cwd=cwd)

    after = subprocess.run(["docker", "system", "df"], capture_output=True, text=True)
    _notify(after.stdout.strip())
    _notify(f"host free: {_free()}")
    raise SystemExit(0)
