import angreal
import subprocess
import os
import webbrowser

cwd = os.path.join(angreal.get_root(), '..')
docs = angreal.command_group(name="docs", about="commands for documentation")


@docs()
@angreal.command(name="build", about="generate API docs and build the site")
def build_docs():
    # Generate API docs with plissken
    print("Generating API documentation with plissken...")
    result = subprocess.run(["plissken", "render"], cwd=cwd)
    if result.returncode != 0:
        print("Warning: plissken render failed (is plissken installed?)")

    # Build mkdocs site
    print("Building documentation site...")
    result = subprocess.run(["mkdocs", "build"], cwd=cwd)
    raise SystemExit(result.returncode)


@docs()
@angreal.command(name="serve", about="serve documentation locally with live reload")
@angreal.argument(name="port", long="port", short="p", required=False, help="port to serve on (default: 8000)")
def serve_docs(port="8000"):
    # Generate API docs first
    print("Generating API documentation with plissken...")
    subprocess.run(["plissken", "render"], cwd=cwd)

    # Serve with mkdocs
    print(f"Serving docs at http://localhost:{port}")
    result = subprocess.run(["mkdocs", "serve", "-a", f"localhost:{port}"], cwd=cwd)
    raise SystemExit(result.returncode)


@docs()
@angreal.command(name="api", about="regenerate API docs only (plissken render)")
def api_docs():
    result = subprocess.run(["plissken", "render"], cwd=cwd)
    raise SystemExit(result.returncode)
