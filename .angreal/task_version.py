import angreal
import subprocess
import os
import re
import json

cwd = os.path.join(angreal.get_root(), '..')
version_group = angreal.command_group(name="version", about="commands for version management")

WORKSPACE_CARGO = os.path.join(cwd, "Cargo.toml")


def read_version():
    """Read the current workspace version from root Cargo.toml."""
    with open(WORKSPACE_CARGO, "r") as f:
        content = f.read()
    match = re.search(r'\[workspace\.package\]\s*\n(?:.*\n)*?version\s*=\s*"([^"]+)"', content)
    if not match:
        raise ValueError("Could not find workspace.package.version in Cargo.toml")
    return match.group(1)


def write_version(new_version):
    """Update the workspace version in root Cargo.toml."""
    with open(WORKSPACE_CARGO, "r") as f:
        content = f.read()

    content = re.sub(
        r'(\[workspace\.package\]\s*\n(?:.*\n)*?version\s*=\s*")[^"]+(")',
        rf'\g<1>{new_version}\2',
        content,
    )

    with open(WORKSPACE_CARGO, "w") as f:
        f.write(content)

    # Update tauri.conf.json and package.json if they exist
    for root, dirs, files in os.walk(cwd):
        # Skip node_modules and target directories
        dirs[:] = [d for d in dirs if d not in ("node_modules", "target", ".git")]
        for fname in files:
            if fname == "tauri.conf.json":
                path = os.path.join(root, fname)
                with open(path, "r") as f:
                    config = json.load(f)
                if "version" in config:
                    config["version"] = new_version
                    with open(path, "w") as f:
                        json.dump(config, f, indent=2)
                        f.write("\n")
                    print(f"  Updated {os.path.relpath(path, cwd)}")
            elif fname == "package.json":
                path = os.path.join(root, fname)
                with open(path, "r") as f:
                    pkg = json.load(f)
                if pkg.get("private") and "version" in pkg:
                    pkg["version"] = new_version
                    with open(path, "w") as f:
                        json.dump(pkg, f, indent=2)
                        f.write("\n")
                    print(f"  Updated {os.path.relpath(path, cwd)}")

    # Update bindings Cargo.toml files (standalone workspaces, version not inherited)
    bindings_dir = os.path.join(cwd, "bindings")
    if os.path.isdir(bindings_dir):
        for binding in os.listdir(bindings_dir):
            cargo_path = os.path.join(bindings_dir, binding, "Cargo.toml")
            if os.path.isfile(cargo_path):
                with open(cargo_path, "r") as f:
                    content = f.read()
                updated = re.sub(
                    r'(\[package\]\s*\n(?:.*\n)*?version\s*=\s*")[^"]+(")',
                    rf'\g<1>{new_version}\2',
                    content,
                )
                if updated != content:
                    with open(cargo_path, "w") as f:
                        f.write(updated)
                    print(f"  Updated {os.path.relpath(cargo_path, cwd)}")


def bump(version, part):
    """Bump a semver version string."""
    major, minor, patch = [int(x) for x in version.split(".")]
    if part == "major":
        return f"{major + 1}.0.0"
    elif part == "minor":
        return f"{major}.{minor + 1}.0"
    elif part == "patch":
        return f"{major}.{minor}.{patch + 1}"
    else:
        raise ValueError(f"Unknown version part: {part}")


def find_all_versions():
    """Find all version declarations across the project."""
    versions = {}
    workspace_version = read_version()
    versions["Cargo.toml (workspace)"] = workspace_version

    for root, dirs, files in os.walk(cwd):
        dirs[:] = [d for d in dirs if d not in ("node_modules", "target", ".git")]
        for fname in files:
            path = os.path.join(root, fname)
            rel = os.path.relpath(path, cwd)
            if fname == "tauri.conf.json":
                with open(path, "r") as f:
                    config = json.load(f)
                if "version" in config:
                    versions[rel] = config["version"]
            elif fname == "package.json":
                with open(path, "r") as f:
                    pkg = json.load(f)
                if pkg.get("private") and "version" in pkg:
                    versions[rel] = pkg["version"]

    # Check bindings Cargo.toml files
    bindings_dir = os.path.join(cwd, "bindings")
    if os.path.isdir(bindings_dir):
        for binding in os.listdir(bindings_dir):
            cargo_path = os.path.join(bindings_dir, binding, "Cargo.toml")
            if os.path.isfile(cargo_path):
                with open(cargo_path, "r") as f:
                    content = f.read()
                match = re.search(r'\[package\]\s*\n(?:.*\n)*?version\s*=\s*"([^"]+)"', content)
                if match:
                    rel = os.path.relpath(cargo_path, cwd)
                    versions[rel] = match.group(1)

    return versions


@version_group()
@angreal.command(name="show", about="show current version")
def show_version():
    version = read_version()
    print(f"skadi v{version}")


@version_group()
@angreal.command(name="verify", about="check all version declarations are in sync")
def verify_versions():
    versions = find_all_versions()
    workspace_version = read_version()
    all_match = True

    for source, version in versions.items():
        status = "ok" if version == workspace_version else "MISMATCH"
        if version != workspace_version:
            all_match = False
        print(f"  {status:8s}  {version:10s}  {source}")

    if all_match:
        print(f"\nAll versions in sync: {workspace_version}")
    else:
        print(f"\nExpected: {workspace_version} -- run 'angreal version bump' to fix")
        raise SystemExit(1)


@version_group()
@angreal.command(name="bump", about="bump the project version")
@angreal.argument(name="part", required=True, help="version part to bump (major, minor, patch)")
def bump_version(part):
    current = read_version()
    new = bump(current, part)
    print(f"Bumping version: {current} -> {new}")
    write_version(new)
    print(f"Updated workspace version to {new}")

    # Verify the workspace is consistent
    result = subprocess.run(["cargo", "check", "--workspace"], cwd=cwd, capture_output=True)
    if result.returncode != 0:
        print("Warning: cargo check failed after version bump")
        print(result.stderr.decode())
