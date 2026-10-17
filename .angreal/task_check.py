import angreal
import subprocess
import os
import glob
import re

cwd = os.path.join(angreal.get_root(), '..')
check = angreal.command_group(name="check", about="commands for code quality checks")


# The two crates outside the workspace (SKADI-T-0640): `--workspace` and a bare
# `cargo fmt` never reach them, so every sweep names them. The web crate is
# built for wasm and is linted for that target.
EXCLUDED_MANIFESTS = [
    "crates/skadi-web/Cargo.toml",
    "crates/skadi-downloader-worker/Cargo.toml",
]
WEB_MANIFEST = "crates/skadi-web/Cargo.toml"
WASM_TARGET = "wasm32-unknown-unknown"

FMT_COMMANDS = [["cargo", "fmt", "--check"]] + [
    ["cargo", "fmt", "--check", "--manifest-path", m] for m in EXCLUDED_MANIFESTS
]
CLIPPY_COMMANDS = [
    ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
    ["cargo", "clippy", "--manifest-path", "crates/skadi-downloader-worker/Cargo.toml",
     "--all-targets", "--", "-D", "warnings"],
    ["cargo", "clippy", "--manifest-path", WEB_MANIFEST, "--target", WASM_TARGET,
     "--all-targets", "--", "-D", "warnings"],
]


def _run_all(commands):
    for cmd in commands:
        result = subprocess.run(cmd, cwd=cwd)
        if result.returncode != 0:
            raise SystemExit(result.returncode)


@check()
@angreal.command(name="fmt", about="check code formatting (workspace, web and worker)")
def fmt_check():
    _run_all(FMT_COMMANDS)


@check()
@angreal.command(name="clippy", about="run clippy lints (workspace, worker, web on wasm)")
def clippy_check():
    _run_all(CLIPPY_COMMANDS)


# Command names are safe to repeat — angreal registers the decorated function
# object per group — so only module-level helpers and constants are checked.
_TASK_COMMAND_NAMES = {
    "up", "down", "build", "redeploy", "logs", "status", "psql", "test", "itest",
    "reset", "schema", "push", "publish_apk",
}


def _task_module_collisions():
    """Module-level names defined by more than one `.angreal/task_*.py`.

    Angreal executes every task file into ONE shared namespace, so a helper or
    constant defined twice silently resolves to whichever file loaded last. That
    is not theoretical: `_compose`/`PROJECT` collisions made `angreal lab up` and
    `angreal db up` operate on the PRODUCTION compose project — starting the live
    stack, and prompting to recreate the prod database volume (SKADI-T-0533).
    """
    root = os.path.join(angreal.get_root())
    seen = {}
    collisions = {}
    for path in sorted(glob.glob(os.path.join(root, "task_*.py"))):
        name = os.path.basename(path)
        with open(path) as fh:
            body = fh.read()
        for m in re.finditer(r"^(?:def (\w+)|([A-Z_][A-Z0-9_]*) *=)", body, re.M):
            sym = m.group(1) or m.group(2)
            if sym in _TASK_COMMAND_NAMES:
                continue
            if sym in seen and seen[sym] != name:
                collisions.setdefault(sym, {seen[sym]}).add(name)
            seen.setdefault(sym, name)
    return collisions


@check()
@angreal.command(
    name="tasks",
    about="check the angreal task modules for colliding module-level names",
)
def task_namespace_check():
    collisions = _task_module_collisions()
    if not collisions:
        print("angreal task modules: no module-level name collisions")
        return 0
    print("Colliding module-level names across .angreal/task_*.py:")
    for sym, files in sorted(collisions.items()):
        print(f"  {sym:24} {', '.join(sorted(files))}")
    print(
        "\nEvery task file shares one namespace, so the last definition wins. "
        "Prefix these per module (SKADI-T-0533)."
    )
    raise SystemExit(1)


@check()
@angreal.command(name="all", about="run all checks (fmt + clippy + schema drift)")
def all_checks():
    for cmd in FMT_COMMANDS + CLIPPY_COMMANDS + [
        ["cargo", "check", "--workspace"],
        # Generated schema must match the logical DDL (SKADI-I-0015). No-ops
        # cleanly if the diesel-dualdb-schema CLI isn't available.
        ["angreal", "db", "schema", "--check"],
        # Colliding helper names across task modules have silently pointed dev
        # commands at the production stack before (SKADI-T-0533).
        ["angreal", "check", "tasks"],
    ]:
        result = subprocess.run(cmd, cwd=cwd)
        if result.returncode != 0:
            raise SystemExit(result.returncode)


@check()
@angreal.command(
    name="compose",
    about="render base and base+lab compose with placeholder env",
)
def compose_check():
    """Validate the deploy compose files (SKADI-T-0485).

    Nothing validated them before: a broken overlay key or a typo'd
    interpolation surfaced at deploy time, on the machine being deployed to.

    Delegates to `deploy/compose-check.sh` so CI and this task cannot drift —
    the script is the single definition of what "renders" means.
    """
    result = subprocess.run([os.path.join(cwd, "deploy", "compose-check.sh")], cwd=cwd)
    raise SystemExit(result.returncode)
