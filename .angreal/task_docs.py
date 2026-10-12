"""
Docs tasks (SKADI-T-0715). The docs are an Astro Starlight site in docs/ with
the Colliery theme starlight-aurora. They need Node 22 or later. The API
reference is written by plissken into docs/src/content/docs/reference/api/.

Every task file shares one namespace (SKADI-T-0533): module-level names here
carry a `_docs` prefix.
"""

import os
import shutil
import subprocess

import angreal

cwd = os.path.join(angreal.get_root(), '..')
_DOCS_DIR = os.path.join(cwd, "docs")

docs = angreal.command_group(name="docs", about="commands for documentation")


def _docs_plissken():
    """Write the API reference. A missing plissken is a warning, not an error."""
    if not shutil.which("plissken"):
        print("Warning: plissken not found; the site builds with no API reference "
              "(cargo install plissken --version 0.1.0)")
        return 0
    print("Generating API documentation with plissken...")
    return subprocess.run(["plissken", "render"], cwd=cwd).returncode


def _docs_npm(*args):
    npm = shutil.which("npm")
    if not npm:
        print("npm not found: install Node 22 or later")
        return 1
    if not os.path.isdir(os.path.join(_DOCS_DIR, "node_modules")):
        result = subprocess.run([npm, "ci", "--no-audit", "--no-fund"], cwd=_DOCS_DIR)
        if result.returncode != 0:
            return result.returncode
    return subprocess.run([npm, *args], cwd=_DOCS_DIR).returncode


@docs()
@angreal.command(
    name="build",
    about="generate API docs and build the site into docs/dist",
    tool=angreal.ToolDescription(
        "Runs `plissken render`, then builds the Starlight site in docs/ into "
        "docs/dist. Installs the npm dependencies on the first run. Fails when "
        "a page does not build.",
        risk_level="safe",
    ),
)
def build_docs():
    result = _docs_plissken()
    if result != 0:
        raise SystemExit(result)
    raise SystemExit(_docs_npm("run", "build"))


@docs()
@angreal.command(
    name="serve",
    about="serve the docs site with live reload",
    tool=angreal.ToolDescription(
        "Runs `plissken render`, then serves the docs site at "
        "http://localhost:4321 and reloads on each change. In a terminal it runs "
        "until Ctrl+C. From a script or an agent, Astro runs it in the "
        "background: stop it with `npx astro dev stop` in docs/.",
        risk_level="safe",
    ),
)
def serve_docs():
    _docs_plissken()
    raise SystemExit(_docs_npm("run", "dev"))


@docs()
@angreal.command(name="api", about="regenerate API docs only (plissken render)")
def api_docs():
    raise SystemExit(subprocess.run(["plissken", "render"], cwd=cwd).returncode)
