import angreal
import subprocess
import os
import sys

cwd = os.path.join(angreal.get_root(), '..')
test = angreal.command_group(name="test", about="commands for running tests")


def _notify(msg):
    """Print an operator-facing message so it actually reaches the terminal.

    A bare `print()` from an angreal task is lost: angreal embeds the Python
    interpreter, and a task that ends in `raise SystemExit` exits without
    stdout being flushed — so a prerequisite failure arrived as a bare non-zero
    exit with no stated cause (SKADI-T-0382). stderr is unbuffered here and is
    the right stream for diagnostics anyway; the explicit flush covers the
    exit path regardless.
    """
    print(msg, file=sys.stderr, flush=True)

# Web UI (Leptos/wasm) test crate — built by trunk/wasm, not part of the cargo
# workspace; tested via wasm-pack in a headless browser (SKADI-T-0118+).
WEB_CRATE = os.path.join("crates", "skadi-web")
_CFT_JSON = "https://googlechromelabs.github.io/chrome-for-testing/known-good-versions-with-downloads.json"
_CHROME_BINARIES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome", "google-chrome-stable", "chromium", "chromium-browser",
]


def _chrome_version():
    """(full, major) of the installed Chrome/Chromium, or (None, None)."""
    import re
    for b in _CHROME_BINARIES:
        try:
            out = subprocess.run([b, "--version"], capture_output=True, text=True)
        except (FileNotFoundError, OSError):
            continue
        if out.returncode == 0:
            m = re.search(r"(\d+)\.\d+\.\d+\.\d+", out.stdout)
            if m:
                return m.group(0), m.group(1)
    return None, None


def _cft_platform():
    import platform
    s, m = platform.system(), platform.machine()
    if s == "Darwin":
        return "mac-arm64" if m == "arm64" else "mac-x64"
    if s == "Linux":
        return "linux64"
    if s == "Windows":
        return "win64"
    return None


def _ensure_chromedriver():
    """Return a path to a chromedriver matching the installed Chrome's MAJOR
    version, downloading it from Chrome-for-Testing (cached by major) if needed.
    wasm-pack ignores $CHROMEDRIVER, so this is passed via --chromedriver.
    Returns None (with a printed hint) if Chrome isn't found / no match exists;
    the caller then lets wasm-pack manage its own driver (may mismatch)."""
    import json
    import platform
    import shutil
    import stat
    import urllib.request
    import zipfile

    full, major = _chrome_version()
    if not major:
        _notify("Chrome/Chromium not found — letting wasm-pack manage chromedriver.")
        return None
    plat = _cft_platform()
    if not plat:
        return None
    cache_dir = os.path.join(os.path.expanduser("~"), ".local", "share", "skadi-chromedriver", major)
    driver = os.path.join(cache_dir, "chromedriver")
    if os.path.isfile(driver):
        return driver

    try:
        with urllib.request.urlopen(_CFT_JSON, timeout=30) as r:
            data = json.load(r)
    except Exception as e:  # noqa: BLE001 — best-effort tooling
        _notify(f"Could not reach Chrome-for-Testing ({e}); letting wasm-pack manage chromedriver.")
        return None
    match = None
    for v in data["versions"]:
        if v["version"].startswith(major + "."):
            for d in v.get("downloads", {}).get("chromedriver", []):
                if d["platform"] == plat:
                    match = (v["version"], d["url"])  # keep last == latest build
    if not match:
        _notify(f"No Chrome-for-Testing chromedriver for Chrome {full} ({plat}).")
        return None

    ver, url = match
    print(f"Fetching chromedriver {ver} for Chrome {full} ({plat})…")
    os.makedirs(cache_dir, exist_ok=True)
    zpath = os.path.join(cache_dir, "cd.zip")
    urllib.request.urlretrieve(url, zpath)
    with zipfile.ZipFile(zpath) as z:
        member = next((m for m in z.namelist() if m.endswith("/chromedriver")), None)
        if member is None:
            os.remove(zpath)
            return None
        with z.open(member) as src, open(driver, "wb") as dst:
            shutil.copyfileobj(src, dst)
    os.remove(zpath)
    os.chmod(driver, os.stat(driver).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    if platform.system() == "Darwin":
        subprocess.run(["xattr", "-dr", "com.apple.quarantine", driver], capture_output=True)
    return driver


@test()
@angreal.command(
    name="e2e-web",
    about="run the Playwright browser E2E (real UI vs a mock-provider daemon)",
    tool=angreal.ToolDescription(
        """
        Drive the built web UI in a headless browser, end-to-end, against the
        mock-provider harness daemon (add → search → grab → import → dashboard).

        ## When to use
        - After changing the web UI or the steer-it backend, for a full-stack check.
        - NOT part of `test all` — needs Node + Playwright + trunk + a Chrome.

        ## Notes
        - Installs npm deps + Playwright's Chromium on first run.
        - The Playwright webServer runs `web-e2e/run-harness.sh` (trunk build +
          `cargo run -p skadi-e2e-harness --features embed-ui`).
        """,
        risk_level="safe",
    ),
)
def e2e_web():
    import shutil
    if shutil.which("npx") is None:
        _notify("Node/npx not found. Install Node.js to run the web E2E.")
        raise SystemExit(1)
    e2e_dir = os.path.join(cwd, "web-e2e")
    if not os.path.isdir(os.path.join(e2e_dir, "node_modules")):
        if subprocess.run(["npm", "install"], cwd=e2e_dir).returncode != 0:
            raise SystemExit(1)
    # Idempotent; no-op if the browser is already present.
    subprocess.run(["npx", "playwright", "install", "chromium"], cwd=e2e_dir)
    result = subprocess.run(["npx", "playwright", "test"], cwd=e2e_dir)
    raise SystemExit(result.returncode)


@test()
@angreal.command(
    name="web",
    about="run the web UI wasm tests (headless Chrome via wasm-pack)",
    tool=angreal.ToolDescription(
        """
        Run the skadi-web (Leptos/wasm) test suites in a headless browser.

        ## When to use
        - After changing crates/skadi-web (logic helpers, components).
        - NOT part of `test all` — needs wasm-pack + a Chrome install.

        ## Notes
        - Requires `cargo install wasm-pack` and Google Chrome.
        - A chromedriver matching Chrome's major version is fetched
          automatically (cached under ~/.local/share/skadi-chromedriver).
        """,
        risk_level="safe",
    ),
)
def web_tests():
    import shutil
    if shutil.which("wasm-pack") is None:
        _notify("wasm-pack not found. Install with: cargo install wasm-pack")
        raise SystemExit(1)
    driver = _ensure_chromedriver()
    cmd = ["wasm-pack", "test", "--headless", "--chrome"]
    if driver:
        cmd += ["--chromedriver", driver]
    cmd.append(WEB_CRATE)
    result = subprocess.run(cmd, cwd=cwd)
    raise SystemExit(result.returncode)


def get_crates():
    """Discover crates in the workspace."""
    crates_dir = os.path.join(cwd, 'crates')
    if not os.path.isdir(crates_dir):
        return []
    return [d for d in os.listdir(crates_dir)
            if os.path.isfile(os.path.join(crates_dir, d, 'Cargo.toml'))]


def _run(cmd):
    """Run a command and exit with its return code."""
    result = subprocess.run(cmd, cwd=cwd)
    raise SystemExit(result.returncode)


def _add_crate_filter(cmd, crate_name, filter_str):
    """Add crate and filter arguments to a cargo command."""
    if crate_name:
        cmd.extend(["-p", crate_name])
    cmd.extend(["--", "--test-threads=1"])
    if filter_str:
        cmd.append(filter_str)
    return cmd


@test()
@angreal.command(name="unit", about="run unit tests (cargo test --lib)")
@angreal.argument(name="crate_name", required=False, help="specific crate to test (default: all)")
@angreal.argument(name="filter", long="filter", short="f", required=False, help="filter for specific tests")
def unit_tests(crate_name="", filter=""):
    cmd = ["cargo", "test", "--lib", "-v"]
    _run(_add_crate_filter(cmd, crate_name, filter))


@test()
@angreal.command(name="integration", about="run integration tests (tests/integration.rs)")
@angreal.argument(name="crate_name", required=False, help="specific crate to test (default: all)")
@angreal.argument(name="filter", long="filter", short="f", required=False, help="filter for specific tests")
def integration_tests(crate_name="", filter=""):
    cmd = ["cargo", "test", "--test", "integration"]
    _run(_add_crate_filter(cmd, crate_name, filter))


@test()
@angreal.command(name="functional", about="run functional tests (tests/functional.rs)")
@angreal.argument(name="crate_name", required=False, help="specific crate to test (default: all)")
@angreal.argument(name="filter", long="filter", short="f", required=False, help="filter for specific tests")
@angreal.argument(
    name="ignored",
    long="ignored",
    takes_value=False,
    help="include #[ignore] tests (e.g. tests needing external services)",
)
def functional_tests(crate_name="", filter="", ignored=False):
    cmd = ["cargo", "test", "--test", "functional"]
    if crate_name:
        cmd.extend(["-p", crate_name])
    cmd.extend(["--", "--test-threads=1"])
    if ignored:
        cmd.append("--ignored")
    if filter:
        cmd.append(filter)
    _run(cmd)


@test()
@angreal.command(name="all", about="run all tests (unit, integration, and functional)")
@angreal.argument(name="crate_name", required=False, help="specific crate to test (default: all)")
@angreal.argument(
    name="ignored",
    long="ignored",
    takes_value=False,
    help="include #[ignore] tests",
)
def all_tests(crate_name="", ignored=False):
    cmd = ["cargo", "test", "-v"]
    if crate_name:
        cmd.extend(["-p", crate_name])
    cmd.extend(["--", "--test-threads=1"])
    if ignored:
        cmd.append("--ignored")
    _run(cmd)


@test()
@angreal.command(name="coverage", about="generate code coverage report")
@angreal.argument(
    name="type",
    long="type",
    short="t",
    required=False,
    help="test type to measure: unit, integration, functional, all (default: all)",
)
@angreal.argument(
    name="open",
    long="open",
    short="o",
    takes_value=False,
    help="open HTML report in browser",
)
def coverage(type="all", open=False):
    import webbrowser

    output_dir = os.path.join(cwd, "coverage")

    # Build the test selection flags for cargo-llvm-cov
    test_flags = []
    if type == "unit":
        test_flags = ["--lib"]
    elif type == "integration":
        test_flags = ["--test", "integration"]
    elif type == "functional":
        test_flags = ["--test", "functional"]
    # "all" uses no flags (runs everything)

    # Generate coverage
    run_cmd = ["cargo", "llvm-cov", "--workspace", "--no-report"] + test_flags + ["--", "--test-threads=1"]
    report_html = ["cargo", "llvm-cov", "report", "--workspace", "--html", "--output-dir", output_dir]
    report_lcov = ["cargo", "llvm-cov", "report", "--workspace", "--lcov", "--output-path", os.path.join(output_dir, "lcov.info")]
    report_text = ["cargo", "llvm-cov", "report", "--workspace"]

    for cmd in [run_cmd, report_html, report_lcov, report_text]:
        result = subprocess.run(cmd, cwd=cwd)
        if result.returncode != 0:
            raise SystemExit(result.returncode)

    if open:
        report = os.path.join(output_dir, "html", "index.html")
        webbrowser.open(f"file://{os.path.realpath(report)}")


# Crates that carry a cucumber BDD runner (tests/bdd.rs, SKADI-I-0057). The worker
# crate is outside the workspace and needs its own manifest.
BDD_CRATES = [
    "skadi-quality", "skadi-hunter", "skadi-core", "skadi-movies", "skadi-tv",
    "skadi-audiobooks", "skadi-importer", "skadi-naming", "skadi-media-probe",
    "skadi-indexers", "skadi-cardigann", "skadi-downloaders", "skadi-metadata",
    "skadi-notify", "skadi-api", "skadi-store", "skadi-config", "skadi-http",
    "skadi-cli", "skadi-client",
]
BDD_WORKER_MANIFEST = os.path.join(cwd, "crates", "skadi-downloader-worker", "Cargo.toml")


@test()
@angreal.command(
    name="bdd",
    about="run the Gherkin/cucumber BDD suites (tests/features) — @gap/@lab scenarios skipped unless asked",
    tool=angreal.ToolDescription(
        """
        Run each crate's cucumber runner (`tests/bdd.rs`) over its `tests/features`.
        Scenarios are tagged @passing (verified behaviour), @gap (expected *arr
        behaviour the code lacks — fails by design), @bug (known-wrong behaviour),
        @lab (needs `angreal lab up`). By default @gap, @bug and @lab are skipped so the
        suite is green; `--gaps` / `--lab` include them (SKADI-I-0057).

        ## When to use
        - After changing acquisition/domain/importer logic, or when reviewing a
          component: `angreal test bdd --crate skadi-hunter`.
        - `angreal test bdd --gaps --crate <crate>` lists what is still missing
          (expect failures — that is the point).
        """,
        risk_level="safe",
    ),
)
@angreal.argument(name="crate_name", long="crate", short="c", takes_value=True, required=False,
                  help="one crate (default: every crate with a BDD runner)")
@angreal.argument(name="gaps", long="gaps", takes_value=False, is_flag=True,
                  help="also run @gap and @bug scenarios (they are expected to fail)")
@angreal.argument(name="lab", long="lab", takes_value=False, is_flag=True,
                  help="also run @lab scenarios (need `angreal lab up`)")
def bdd_tests(crate_name="", gaps=False, lab=False):
    env = dict(os.environ)
    if gaps:
        env["SKADI_BDD_GAPS"] = "1"
    if lab:
        env["SKADI_BDD_LAB"] = "1"
    targets = [crate_name] if crate_name else BDD_CRATES + ["skadi-downloader-worker"]
    rc = 0
    for c in targets:
        if c == "skadi-downloader-worker":
            cmd = ["cargo", "test", "--manifest-path", BDD_WORKER_MANIFEST, "--test", "bdd"]
        else:
            cmd = ["cargo", "test", "-p", c, "--test", "bdd"]
        print(f"==> {c}")
        r = subprocess.run(cmd, cwd=cwd, env=env).returncode
        if r != 0:
            rc = r
            if not gaps and not crate_name:
                print(f"    {c}: FAILED")
    raise SystemExit(rc)


# --- batched full verification (SKADI-T-0551) -------------------------------

# Crates excluded from the cargo workspace (see the root Cargo.toml): the web UI
# is a wasm/trunk target and the download worker embeds librqbit's large tree.
# `cargo test --workspace` never builds either, which is how skadi-web's test
# files sat uncompilable for several commits with nothing reporting it.
EXCLUDED_CRATES = ["skadi-web", "skadi-downloader-worker"]

# Per-batch wall-clock budget, covering **execution only** — the workspace is
# compiled once up front (see `_prebuild`). Without that split the first batch
# pays for building the entire dependency tree and times out on work that is not
# its own, while later batches get a free ride on the warm target dir: the
# budget then measures build order rather than test cost, which is useless.
BATCH_TIMEOUT_SECS = 1200

# Compiling the workspace's test binaries from cold is the bulk of a full run and
# is not something a per-batch timeout should be policing.
BUILD_TIMEOUT_SECS = 2700


def _crate_batches(size):
    """Workspace crates, in fixed (sorted) order, chunked into batches."""
    crates = sorted(c for c in get_crates() if c not in EXCLUDED_CRATES)
    return [crates[i:i + size] for i in range(0, len(crates), size)]


def _summarise(out):
    """(failed_suites, failed_bdd_scenarios, compile_errors) from cargo output.

    Three separate signals because they fail differently and one of them used to
    be invisible: a cucumber suite that reported failing scenarios still exited
    0 until SKADI-T-0536, so grepping only for `test result: FAILED` reported a
    broken BDD suite as green. Compile errors are counted too — a batch that
    never built produced no `test result:` lines at all, which naive counting
    reads as success.
    """
    import re
    failed = len(re.findall(r"^test result: FAILED", out, re.M))
    bdd = len(re.findall(r"scenarios \(\d+ passed, \d+ failed\)", out))
    errs = len(re.findall(r"^error(\[|:)", out, re.M))
    passed = len(re.findall(r"^test result: ok", out, re.M))
    return failed, bdd, errs, passed


@test()
@angreal.command(
    name="verify",
    about="run the whole suite in batches with per-batch timeouts (SKADI-T-0551)",
)
@angreal.argument(
    name="batch_size",
    long="batch-size",
    required=False,
    help="crates per cargo invocation (default 4)",
)
@angreal.argument(
    name="skip_excluded",
    long="skip-excluded",
    takes_value=False,
    help="don't compile the non-workspace crates (skadi-web, skadi-downloader-worker)",
)
def verify(batch_size="", skip_excluded=False):
    """Run the full test suite in bounded batches and report honestly.

    `cargo test --workspace` no longer fits in one sensible timeout: a single
    invocation runs for well over an hour, and when the timeout fires the run is
    killed partway with most suites green and the rest simply never executed. The
    danger is not the wasted time, it is that the partial output *looks* clean —
    zero failures, because the failing thing never ran.

    So: fixed-size batches, each with its own budget, and **a timeout is counted
    as a failure**, named in the summary. A batch that did not finish is reported
    as not-finished rather than folded into the pass count.
    """
    size = int(batch_size) if batch_size else 4
    batches = _crate_batches(size)
    results = []

    # Build once, so each batch's budget measures how long its tests take rather
    # than how much of the dependency tree happened to be cold when it ran.
    _notify("building test binaries (once, up front)...")
    try:
        build = subprocess.run(
            ["cargo", "test", "--workspace", "--no-run"],
            cwd=cwd, capture_output=True, text=True, timeout=BUILD_TIMEOUT_SECS,
        )
        if build.returncode != 0:
            for line in (build.stdout + build.stderr).splitlines():
                if line.startswith("error"):
                    _notify(f"    {line}")
            _notify("build failed — nothing was run")
            raise SystemExit(1)
    except subprocess.TimeoutExpired:
        _notify(f"build timed out after {BUILD_TIMEOUT_SECS}s — nothing was run")
        raise SystemExit(1)
    _notify("build done\n")

    for i, batch in enumerate(batches, 1):
        cmd = ["cargo", "test"]
        for c in batch:
            cmd.extend(["-p", c])
        _notify(f"[{i}/{len(batches)}] {' '.join(batch)}")
        try:
            proc = subprocess.run(
                cmd, cwd=cwd, capture_output=True, text=True,
                timeout=BATCH_TIMEOUT_SECS,
            )
            out = proc.stdout + proc.stderr
            failed, bdd, errs, passed = _summarise(out)
            ok = proc.returncode == 0 and failed == 0 and bdd == 0 and errs == 0
            results.append((batch, ok, passed, failed, bdd, errs, None))
            if not ok:
                # Print the failing detail; a summary line alone sends the
                # operator back to re-run the batch by hand to see anything.
                for line in out.splitlines():
                    if ("test result: FAILED" in line
                            or "scenarios (" in line
                            or line.startswith("error")
                            or "panicked at" in line):
                        _notify(f"    {line}")
        except subprocess.TimeoutExpired:
            results.append((batch, False, 0, 0, 0, 0, "timed out"))
            _notify(f"    TIMED OUT after {BATCH_TIMEOUT_SECS}s — nothing in this "
                    f"batch is verified")

    # The excluded crates are compiled even when not run: they are outside the
    # workspace, so nothing else ever builds their tests, and a test file that no
    # longer compiles is indistinguishable from one that passes.
    if not skip_excluded:
        for crate in EXCLUDED_CRATES:
            path = os.path.join(cwd, "crates", crate)
            if not os.path.isdir(path):
                continue
            _notify(f"[check] {crate} (outside the workspace — compile only)")
            cmd = ["cargo", "check", "--tests"]
            if crate == "skadi-web":
                cmd.extend(["--target", "wasm32-unknown-unknown"])
            try:
                proc = subprocess.run(
                    cmd, cwd=path, capture_output=True, text=True,
                    timeout=BATCH_TIMEOUT_SECS,
                )
                ok = proc.returncode == 0
                results.append(([crate], ok, 0, 0, 0, 0,
                                None if ok else "does not compile"))
                if not ok:
                    for line in (proc.stdout + proc.stderr).splitlines():
                        if line.startswith("error"):
                            _notify(f"    {line}")
            except subprocess.TimeoutExpired:
                results.append(([crate], False, 0, 0, 0, 0, "timed out"))

    _notify("")
    _notify("=== verify summary ===")
    total_passed = sum(r[2] for r in results)
    bad = [r for r in results if not r[1]]
    for batch, ok, passed, failed, bdd, errs, note in results:
        mark = "ok  " if ok else "FAIL"
        detail = note or f"{passed} suites passed"
        if failed or bdd or errs:
            detail = f"{failed} suites failed, {bdd} bdd, {errs} compile errors"
        _notify(f"  {mark}  {' '.join(batch):<48} {detail}")
    _notify(f"  {total_passed} suites passed across {len(results)} batches")

    if bad:
        _notify(f"\n{len(bad)} batch(es) did not pass — the run is NOT green.")
        raise SystemExit(1)
    _notify("\nall batches green")
    raise SystemExit(0)
