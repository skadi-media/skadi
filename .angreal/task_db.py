import angreal
import os
import subprocess
import time

cwd = os.path.join(angreal.get_root(), '..')

db = angreal.command_group(name="db", about="manage the docker-compose test database (Postgres)")

# Connection URL for the compose `postgres` service (host port 5433).
DB_TEST_DATABASE_URL = "postgres://skadi:skadi@127.0.0.1:5433/skadi"

# Dedicated Compose project + file for the *test* database. Critically, this is
# NOT the same project the live deploy stack (deploy/docker-compose.yml) runs
# under: that one resolves to project "skadi", so a bare `docker compose down -v`
# from the repo root would tear down and delete the live stack's volumes. Pinning
# `-p skadi-test -f docker-compose.yml` keeps every command here scoped to the
# throwaway test Postgres and unable to touch the deploy stack. (SKADI-T-0077)
DB_TEST_PROJECT = "skadi-test"
DB_COMPOSE_FILE = os.path.join(cwd, "docker-compose.yml")
DB_CONTAINER = "skadi-test-postgres-1"


def _DB_compose(args):
    # SKADI-T-0533: see the note in task_lab.py. This module's names used to be
    # shadowed by the deploy/NAS tasks', so `angreal db up` ran against the
    # deploy compose file and Docker offered to recreate the PRODUCTION database
    # volume. Fail loudly rather than address the wrong stack.
    assert DB_TEST_PROJECT == "skadi-test", (
        f"db tasks are pinned to project 'skadi-test', got {DB_TEST_PROJECT!r} — "
        "a module-level name collision (SKADI-T-0533)"
    )
    assert os.path.dirname(DB_COMPOSE_FILE) == cwd, (
        f"db tasks use the repo-root compose file, got {DB_COMPOSE_FILE!r} — "
        "a module-level name collision (SKADI-T-0533)"
    )
    return subprocess.run(
        ["docker", "compose", "-p", DB_TEST_PROJECT, "-f", DB_COMPOSE_FILE] + args,
        cwd=cwd,
    )


def _DB_wait_healthy(timeout_s=30):
    """Block until the test postgres container reports healthy (or time out)."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        health = subprocess.run(
            ["docker", "inspect", "-f", "{{.State.Health.Status}}", DB_CONTAINER],
            capture_output=True, text=True,
        ).stdout.strip()
        if health == "healthy":
            return True
        time.sleep(1)
    return False


def _DB_up_and_wait():
    """Bring the isolated test postgres up (idempotent) and wait for healthy."""
    if _DB_compose(["up", "-d", "postgres"]).returncode != 0:
        raise SystemExit(1)
    if not _DB_wait_healthy():
        print("postgres did not become healthy in time")
        raise SystemExit(1)


@db()
@angreal.command(name="up", about="start the Postgres test database and wait until healthy")
def up():
    _DB_up_and_wait()
    print(f"postgres ready: {DB_TEST_DATABASE_URL}")
    return 0


@db()
@angreal.command(name="down", about="stop and remove the Postgres test database")
def down():
    raise SystemExit(_DB_compose(["down", "-v"]).returncode)


@db()
@angreal.command(
    name="test",
    about="run skadi-store tests against both backends (spins Postgres up/down)",
)
def test():
    """Bring up Postgres, run the dual-backend store tests with
    SKADI_TEST_DATABASE_URL set, then tear it down regardless of outcome."""
    _DB_up_and_wait()
    try:
        env = dict(os.environ, SKADI_TEST_DATABASE_URL=DB_TEST_DATABASE_URL)
        result = subprocess.run(
            ["cargo", "test", "-p", "skadi-store", "--", "--test-threads=1"],
            cwd=cwd, env=env,
        )
    finally:
        _DB_compose(["down", "-v"])
    raise SystemExit(result.returncode)


# Crates whose integration tests use the `skadi-testsupport` per-test-isolated
# database harness, so they run safely in parallel against the shared test
# Postgres. Add crates here as they migrate onto TestDb (SKADI-T-0077).
DB_ISOLATED_CRATES = ["skadi-movies"]


@db()
@angreal.command(
    name="itest",
    about="run the Postgres-default integration tests (brings DB up, leaves it up for reuse)",
)
def itest():
    """Bring up the isolated test Postgres (idempotent) and run the integration
    tests that use the per-test-isolated database harness against it, with
    SKADI_TEST_DATABASE_URL set. Leaves the database running afterwards so it can
    be reused across runs — tear it down explicitly with `angreal db down`."""
    _DB_up_and_wait()
    print(f"running integration tests against {DB_TEST_DATABASE_URL}")
    env = dict(os.environ, SKADI_TEST_DATABASE_URL=DB_TEST_DATABASE_URL)
    cmd = ["cargo", "test"]
    for crate in DB_ISOLATED_CRATES:
        cmd += ["-p", crate]
    result = subprocess.run(cmd, cwd=cwd, env=env)
    raise SystemExit(result.returncode)


# --- diesel-dualdb logical-DDL schema generation (SKADI-I-0015) -------------

# Crates whose schema is generated from logical DDL: (migrations_dir, out_dir).
_SCHEMA_CRATES = ["skadi-store", "skadi-movies", "skadi-audiobooks", "skadi-tv"]


def _migration_versions():
    """Every migration directory across the schema crates, as
    `{version: [(crate, dirname), ...]}`.

    Diesel derives the version from the directory name by stripping non-digits:
    `2026-09-09-000000_book_editions` becomes `20260909000000`.
    """
    import re
    from collections import defaultdict
    seen = defaultdict(list)
    for crate in _SCHEMA_CRATES:
        src = os.path.join(cwd, "crates", crate, "schema", "migrations")
        if not os.path.isdir(src):
            continue
        for name in sorted(os.listdir(src)):
            if not os.path.isdir(os.path.join(src, name)):
                continue
            version = re.sub(r"\D", "", name.split("_", 1)[0])
            if version:
                seen[version].append((crate, name))
    return seen


def _check_version_collisions():
    """Fail on two migrations sharing a version number (SKADI-T-0561).

    Every crate embeds its own migration tree, but they all run against one
    database and therefore share one `__diesel_schema_migrations` table, keyed on
    that version. Two crates adding a migration on the same day at the same
    nominal time collide, and **the second one never runs** — diesel sees the
    version already recorded and skips it. Nothing errors:
    `run_pending_migrations` returns Ok, and the table is simply absent.

    That happened for real (SKADI-T-0550's `item_tags` vs SKADI-T-0448's
    `book_editions`, both `2026-09-09-000000`). The symptom was
    `no such table: book_editions`, which reads like a generator or embedding
    problem and sends you looking in the wrong place entirely. Two branches
    merging would ship it to production.

    Returns the number of collisions found.
    """
    collisions = {v: rows for v, rows in _migration_versions().items() if len(rows) > 1}
    for version, rows in sorted(collisions.items()):
        _notify_db(f"migration version {version} is used more than once:")
        for crate, name in rows:
            _notify_db(f"    {crate}/schema/migrations/{name}")
        _notify_db("  Only the first will ever run — rename one (bump the time "
                   "component, e.g. -010000).")
    return len(collisions)


def _notify_db(msg):
    import sys
    print(msg, file=sys.stderr, flush=True)


def _find_schema_cli():
    """Locate the `diesel-dualdb-schema` binary: PATH first, then the sibling
    diesel-dualdb checkout's debug build. Returns the path or None."""
    from shutil import which
    found = which("diesel-dualdb-schema")
    if found:
        return found
    sibling = os.path.join(cwd, "..", "diesel-dualdb",
                           "target", "debug", "diesel-dualdb-schema")
    if os.path.exists(sibling):
        return sibling
    return None


def _gen_schema(cli, out_root):
    """Regenerate every schema crate's tree under `out_root` (relative subdir
    per crate). Returns 0 on success."""
    for crate in _SCHEMA_CRATES:
        src = os.path.join(cwd, "crates", crate, "schema", "migrations")
        dst = os.path.join(out_root, crate)
        r = subprocess.run([cli, src, dst], cwd=cwd)
        if r.returncode != 0:
            return r.returncode
    return 0


@db()
@angreal.command(
    name="schema",
    about="regenerate the diesel-dualdb schema/migrations from the logical DDL",
)
@angreal.argument(
    name="check", long="check", is_flag=True, takes_value=False,
    help="don't write — fail if the committed generated tree is out of date",
)
def schema(check=False):
    """Regenerate `crates/*/schema/generated` from `crates/*/schema/migrations`
    via the diesel-dualdb-schema CLI. With --check, regenerate to a temp dir and
    diff against the committed tree, failing on drift (CI guard). The CLI lives
    in the sibling diesel-dualdb checkout; if it isn't found this is a no-op with
    a clear message (it can't run until the CLI is published/installed)."""
    # Version collisions are checked first and always — the guard reads only the
    # logical migration trees, so it works even when the generator is missing.
    if _check_version_collisions() > 0:
        raise SystemExit(1)

    cli = _find_schema_cli()
    if cli is None:
        print("diesel-dualdb-schema not found (PATH or ../diesel-dualdb/target/debug). "
              "Install it with `cargo install diesel-dualdb-cli` (the binary is "
              "`diesel-dualdb-schema`), or build the sibling checkout. Skipping.")
        raise SystemExit(0)

    if not check:
        for crate in _SCHEMA_CRATES:
            dst = os.path.join(cwd, "crates", crate, "schema", "generated")
            rc = subprocess.run([cli,
                                 os.path.join(cwd, "crates", crate, "schema", "migrations"),
                                 dst], cwd=cwd).returncode
            if rc != 0:
                raise SystemExit(rc)
        print("regenerated schema/generated for: " + ", ".join(_SCHEMA_CRATES))
        return 0

    # --check: regenerate to a temp tree and diff against the committed one.
    import tempfile, filecmp, difflib
    drift = []
    with tempfile.TemporaryDirectory() as tmp:
        if _gen_schema(cli, tmp) != 0:
            raise SystemExit(1)
        for crate in _SCHEMA_CRATES:
            committed = os.path.join(cwd, "crates", crate, "schema", "generated")
            fresh = os.path.join(tmp, crate)
            r = subprocess.run(["diff", "-r", committed, fresh], capture_output=True, text=True)
            if r.returncode != 0:
                drift.append((crate, r.stdout))
    if drift:
        _notify_db("Schema drift — the committed generated tree does not match the logical DDL.")
        _notify_db("Run `angreal db schema` and commit the result.\n")
        for crate, d in drift:
            _notify_db(f"--- {crate} ---\n{d}")
        raise SystemExit(1)
    _notify_db("schema up to date (committed generated tree matches the logical DDL)")
    # `raise SystemExit(0)`, not `return 0`: angreal treats only `None`/`True` as
    # success, so returning the integer 0 exited **1** — this check had never
    # passed, and its success message never printed either, because a bare
    # `print()` from an angreal task is lost (see `_notify` in task_tests.py).
    raise SystemExit(0)
