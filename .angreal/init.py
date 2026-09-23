
import subprocess
import os
import json


_PROJECT_SLUG = "skadi"
_INCLUDE_UI = "false" == "true"
_INCLUDE_PYTHON_BINDINGS = "false" == "true"
_PROJECT_NAME = "Skadi"
_INITIAL_VERSION = "0.0.1"
_AUTHOR_NAME = "Dylan Storey"
_AUTHOR_EMAIL = "dylanbstorey@users.noreply.github.com"
_LICENSE = "MIT OR Apache-2.0"
_GITHUB_ORG = "skadi-media"



def init():
    import angreal

    project_dir = os.path.dirname(angreal.get_root())
    project_slug = _PROJECT_SLUG
    include_ui = _INCLUDE_UI
    include_python_bindings = _INCLUDE_PYTHON_BINDINGS
    project_name = _PROJECT_NAME
    initial_version = _INITIAL_VERSION
    author_name = _AUTHOR_NAME
    author_email = _AUTHOR_EMAIL
    license_type = _LICENSE
    github_org = _GITHUB_ORG

    print(f"Initializing {project_name}...")

    # Set up git
    subprocess.run(["git", "config", "--global", "init.defaultBranch", "main"],
                   cwd=project_dir, check=True)
    subprocess.run(["git", "init", "."], cwd=project_dir, check=True)

    # Initialize metis if available
    try:
        result = subprocess.run(["metis", "--version"], capture_output=True)
        if result.returncode == 0:
            print("Initializing Metis workspace...")
            subprocess.run(["metis", "init", "--prefix",
                           project_slug.upper().replace("-", "")[:8]],
                          cwd=project_dir, check=False)
    except FileNotFoundError:
        print("Metis not found, skipping workspace initialization.")
        print("  Install metis to enable project management.")

    # Set up UI if requested
    if include_ui:
        print("Setting up Tauri + Vue 3 desktop UI...")
        _scaffold_ui(project_dir, project_slug, project_name, initial_version)

    # Set up Python bindings if requested
    if include_python_bindings:
        print("Setting up PyO3/Maturin Python bindings...")
        _scaffold_python_bindings(
            project_dir, project_slug, project_name, initial_version,
            author_name, author_email, license_type, github_org,
        )
        # Add python section to plissken.toml
        plissken_path = os.path.join(project_dir, "plissken.toml")
        if os.path.isfile(plissken_path):
            with open(plissken_path, "a") as f:
                py_package = project_slug.replace("-", "_")
                f.write(f'\n[python]\npackage = "bindings/{project_slug}-py/python/{py_package}"\n')

    # Install pre-commit hooks
    try:
        subprocess.run(["pre-commit", "install"], cwd=project_dir, check=False)
    except FileNotFoundError:
        print("pre-commit not found, skipping hook installation.")
        print("  Install with: pip install pre-commit")

    # Initial commit (re-add after pre-commit hooks may modify files)
    subprocess.run(["git", "add", "."], cwd=project_dir, check=True)
    result = subprocess.run(["git", "commit", "-m", "initialize project via angreal"],
                           cwd=project_dir)
    if result.returncode != 0:
        # Pre-commit hooks may have modified files, re-stage and retry
        subprocess.run(["git", "add", "."], cwd=project_dir, check=True)
        subprocess.run(["git", "commit", "-m", "initialize project via angreal"],
                       cwd=project_dir, check=True)

    print("")
    print("Project initialized successfully!")
    print("")
    print("Next steps:")
    print(f"  cd {project_slug}")
    print("  cargo check")
    print("  angreal test unit")
    if include_ui:
        print(f"  cd crates/{project_slug}-gui && npm install")
        print("  angreal dev launch")
    if include_python_bindings:
        print(f"  cd bindings/{project_slug}-py && maturin develop")


def _scaffold_ui(project_dir, project_slug, project_name, initial_version):
    """Scaffold a Tauri v2 + Vue 3 + Tailwind desktop app."""
    gui_crate = os.path.join(project_dir, "crates", f"{project_slug}-gui")
    src_tauri = os.path.join(gui_crate, "src-tauri")
    frontend = gui_crate
    slug_underscore = project_slug.replace("-", "_")

    os.makedirs(os.path.join(src_tauri, "src"), exist_ok=True)
    os.makedirs(os.path.join(frontend, "src", "components"), exist_ok=True)
    os.makedirs(os.path.join(frontend, "src", "views"), exist_ok=True)
    os.makedirs(os.path.join(frontend, "src", "stores"), exist_ok=True)
    os.makedirs(os.path.join(frontend, "public"), exist_ok=True)

    # Tauri Cargo.toml
    tauri_cargo = f'''[package]
name = "{project_slug}-gui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
description = "Desktop GUI for {project_slug}"

[lib]
name = "{slug_underscore}_gui"
crate-type = ["lib", "staticlib", "cdylib"]

[build-dependencies]
tauri-build = ''' + '{ version = "2", features = [] }' + f'''

[dependencies]
{project_slug}-core = ''' + '{ workspace = true }' + '''
tauri = { version = "2", features = [] }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
'''
    _write(os.path.join(src_tauri, "Cargo.toml"), tauri_cargo)

    # Tauri build.rs
    _write(os.path.join(src_tauri, "build.rs"), """fn main() {
    tauri_build::build();
}
""")

    # Tauri lib.rs
    _write(os.path.join(src_tauri, "src", "lib.rs"), """mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::greet,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
""")

    # Tauri commands.rs
    _write(os.path.join(src_tauri, "src", "commands.rs"), """#[tauri::command]
pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}
""")

    # Tauri config
    tauri_conf = {
        "productName": project_slug,
        "version": initial_version,
        "identifier": f"io.colliery.{project_slug.replace('-', '')}",
        "build": {
            "frontendDist": "../dist",
            "devUrl": "http://localhost:5173",
            "beforeDevCommand": "npm run dev",
            "beforeBuildCommand": "npm run build"
        },
        "app": {
            "title": project_name,
            "windows": [
                {
                    "title": project_name,
                    "width": 1200,
                    "height": 800
                }
            ]
        }
    }
    _write(os.path.join(src_tauri, "tauri.conf.json"),
           json.dumps(tauri_conf, indent=2) + "\n")

    # Tauri capabilities
    os.makedirs(os.path.join(src_tauri, "capabilities"), exist_ok=True)
    default_cap = {
        "identifier": "default",
        "description": "Default capability",
        "windows": ["main"],
        "permissions": ["core:default"]
    }
    _write(os.path.join(src_tauri, "capabilities", "default.json"),
           json.dumps(default_cap, indent=2) + "\n")

    # Frontend package.json
    package_json = {
        "name": project_slug,
        "private": True,
        "version": initial_version,
        "type": "module",
        "scripts": {
            "dev": "vite",
            "build": "vue-tsc --noEmit && vite build",
            "preview": "vite preview"
        },
        "dependencies": {
            "vue": "^3.5.0",
            "vue-router": "^4.4.0",
            "pinia": "^2.2.0",
            "@tauri-apps/api": "^2.0.0"
        },
        "devDependencies": {
            "@vitejs/plugin-vue": "^5.1.0",
            "typescript": "^5.6.0",
            "vite": "^6.0.0",
            "vue-tsc": "^2.1.0",
            "tailwindcss": "^4.0.0",
            "@tailwindcss/vite": "^4.0.0",
            "vitest": "^2.1.0",
            "@vue/test-utils": "^2.4.0"
        }
    }
    _write(os.path.join(frontend, "package.json"),
           json.dumps(package_json, indent=2) + "\n")

    # Vite config
    _write(os.path.join(frontend, "vite.config.ts"), """import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [vue(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
});
""")

    # TypeScript config
    _write(os.path.join(frontend, "tsconfig.json"), json.dumps({
        "compilerOptions": {
            "target": "ES2021",
            "module": "ESNext",
            "moduleResolution": "bundler",
            "strict": True,
            "jsx": "preserve",
            "resolveJsonModule": True,
            "isolatedModules": True,
            "esModuleInterop": True,
            "lib": ["ES2021", "DOM", "DOM.Iterable"],
            "skipLibCheck": True,
            "noEmit": True,
            "paths": {
                "@/*": ["./src/*"]
            }
        },
        "include": ["src/**/*.ts", "src/**/*.vue"],
        "references": [{"path": "./tsconfig.node.json"}]
    }, indent=2) + "\n")

    _write(os.path.join(frontend, "tsconfig.node.json"), json.dumps({
        "compilerOptions": {
            "composite": True,
            "module": "ESNext",
            "moduleResolution": "bundler",
            "allowSyntheticDefaultImports": True,
            "strict": True
        },
        "include": ["vite.config.ts"]
    }, indent=2) + "\n")

    # index.html
    _write(os.path.join(frontend, "index.html"), f"""<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>{project_name}</title>
  </head>
  <body>
    <div id="app"></div>
    <script type="module" src="/src/main.ts"></script>
  </body>
</html>
""")

    # Vue entry point
    _write(os.path.join(frontend, "src", "main.ts"), """import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";
import "./style.css";

const app = createApp(App);
app.use(createPinia());
app.mount("#app");
""")

    # App.vue - use double curly braces for Vue interpolation
    app_vue = '<script setup lang="ts">\n'
    app_vue += 'import { invoke } from "@tauri-apps/api/core";\n'
    app_vue += 'import { ref } from "vue";\n\n'
    app_vue += 'const greeting = ref("");\n'
    app_vue += 'const name = ref("");\n\n'
    app_vue += 'async function greet() {\n'
    app_vue += '  greeting.value = await invoke("greet", { name: name.value });\n'
    app_vue += '}\n'
    app_vue += '</script>\n\n'
    app_vue += '<template>\n'
    app_vue += '  <main class="flex flex-col items-center justify-center min-h-screen bg-gray-50">\n'
    app_vue += f'    <h1 class="text-3xl font-bold mb-8">{project_name}</h1>\n'
    app_vue += '    <div class="flex gap-2 mb-4">\n'
    app_vue += '      <input\n'
    app_vue += '        v-model="name"\n'
    app_vue += '        class="border rounded px-3 py-2"\n'
    app_vue += '        placeholder="Enter a name..."\n'
    app_vue += '        @keyup.enter="greet"\n'
    app_vue += '      />\n'
    app_vue += '      <button\n'
    app_vue += '        class="bg-blue-600 text-white px-4 py-2 rounded hover:bg-blue-700"\n'
    app_vue += '        @click="greet"\n'
    app_vue += '      >\n'
    app_vue += '        Greet\n'
    app_vue += '      </button>\n'
    app_vue += '    </div>\n'
    app_vue += '    <p v-if="greeting" class="text-lg">{{ greeting }}</p>\n'
    app_vue += '  </main>\n'
    app_vue += '</template>\n'
    _write(os.path.join(frontend, "src", "App.vue"), app_vue)

    # Tailwind CSS
    _write(os.path.join(frontend, "src", "style.css"), """@import "tailwindcss";
""")

    # Vite env types
    _write(os.path.join(frontend, "src", "vite-env.d.ts"), """/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}
""")

    # Build sidecar script
    scripts_dir = os.path.join(project_dir, "scripts")
    os.makedirs(scripts_dir, exist_ok=True)
    sidecar_script = f"""#!/bin/bash
set -euo pipefail

# Build the CLI binary as a Tauri sidecar
TARGET="${{1:-$(rustc -vV | sed -n 's/host: //p')}}"
BINARY_NAME="{project_slug}-$TARGET"

echo "Building sidecar for $TARGET..."
cargo build --release --package {project_slug}-cli --target "$TARGET"

DEST="crates/{project_slug}-gui/src-tauri/binaries"
mkdir -p "$DEST"

if [[ "$TARGET" == *"windows"* ]]; then
    cp "target/$TARGET/release/{project_slug}.exe" "$DEST/$BINARY_NAME.exe"
else
    cp "target/$TARGET/release/{project_slug}" "$DEST/$BINARY_NAME"
fi

echo "Sidecar built: $DEST/$BINARY_NAME"
"""
    _write(os.path.join(scripts_dir, "build-sidecar.sh"), sidecar_script)
    os.chmod(os.path.join(scripts_dir, "build-sidecar.sh"), 0o755)

    # Add sidecar binaries to gitignore
    gitignore_path = os.path.join(project_dir, ".gitignore")
    with open(gitignore_path, "a") as f:
        f.write("\n# Tauri sidecar binaries\n")
        f.write(f"crates/{project_slug}-gui/src-tauri/binaries/\n")
        f.write("\n# Node modules\nnode_modules/\n")

    # Update workspace Cargo.toml to use explicit members (Tauri crate is nested)
    cargo_path = os.path.join(project_dir, "Cargo.toml")
    with open(cargo_path, "r") as f:
        cargo_content = f.read()
    cargo_content = cargo_content.replace(
        'members = ["crates/*"]',
        f'members = [\n'
        f'    "crates/{project_slug}-core",\n'
        f'    "crates/{project_slug}-cli",\n'
        f'    "crates/{project_slug}-gui/src-tauri",\n'
        f']',
    )
    with open(cargo_path, "w") as f:
        f.write(cargo_content)

    print(f"  Created GUI crate at crates/{project_slug}-gui/")
    print(f"  Created sidecar build script at scripts/build-sidecar.sh")
    print("  Run 'npm install' in the GUI crate directory to install frontend dependencies.")


def _scaffold_python_bindings(
    project_dir, project_slug, project_name, initial_version,
    author_name, author_email, license_type, github_org,
):
    """Scaffold a PyO3/Maturin Python bindings crate."""
    slug_underscore = project_slug.replace("-", "_")
    py_package = slug_underscore
    bindings_dir = os.path.join(project_dir, "bindings", f"{project_slug}-py")

    os.makedirs(os.path.join(bindings_dir, "src"), exist_ok=True)
    os.makedirs(os.path.join(bindings_dir, "python", py_package), exist_ok=True)
    os.makedirs(os.path.join(bindings_dir, "tests"), exist_ok=True)

    # Cargo.toml - standalone workspace (excluded from main workspace)
    cargo_toml = f'''[workspace]

[package]
name = "{project_slug}-py"
version = "{initial_version}"
edition = "2024"
license = "{license_type}"
description = "Python bindings for {project_name}"

[lib]
name = "{py_package}"
crate-type = ["cdylib"]

[dependencies]
pyo3 = ''' + '{ version = "0.25", features = ["extension-module", "abi3-py39"] }' + f'''
{project_slug}-core = ''' + '{ path = "../../crates/' + f'{project_slug}-core' + '" }' + '''
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
pythonize = "0.25"
'''
    _write(os.path.join(bindings_dir, "Cargo.toml"), cargo_toml)

    # pyproject.toml
    pyproject = {
        "build-system": {
            "requires": ["maturin>=1.0,<2.0"],
            "build-backend": "maturin",
        },
        "project": {
            "name": project_slug,
            "dynamic": ["version"],
            "description": f"Python bindings for {project_name}",
            "readme": "README.md",
            "authors": [{"name": author_name, "email": author_email}],
            "license": {"text": license_type},
            "classifiers": [
                "Development Status :: 3 - Alpha",
                "Intended Audience :: Developers",
                "Programming Language :: Python :: 3",
                "Programming Language :: Python :: 3.9",
                "Programming Language :: Python :: 3.10",
                "Programming Language :: Python :: 3.11",
                "Programming Language :: Python :: 3.12",
                "Programming Language :: Rust",
            ],
            "requires-python": ">=3.9",
            "dependencies": [],
        },
        "project.urls": {
            "Homepage": f"https://github.com/{github_org}/{project_slug}",
            "Repository": f"https://github.com/{github_org}/{project_slug}",
        },
        "tool.maturin": {
            "module-name": py_package,
            "python-source": "python",
            "strip": True,
        },
    }

    # Write pyproject.toml manually since toml isn't in stdlib
    pyproject_content = f'''[build-system]
requires = ["maturin>=1.0,<2.0"]
build-backend = "maturin"

[project]
name = "{project_slug}"
dynamic = ["version"]
description = "Python bindings for {project_name}"
readme = "README.md"
authors = [
    {{name = "{author_name}", email = "{author_email}"}}
]
license = {{text = "{license_type}"}}
classifiers = [
    "Development Status :: 3 - Alpha",
    "Intended Audience :: Developers",
    "Programming Language :: Python :: 3",
    "Programming Language :: Python :: 3.9",
    "Programming Language :: Python :: 3.10",
    "Programming Language :: Python :: 3.11",
    "Programming Language :: Python :: 3.12",
    "Programming Language :: Rust",
]
requires-python = ">=3.9"
dependencies = []

[project.urls]
Homepage = "https://github.com/{github_org}/{project_slug}"
Repository = "https://github.com/{github_org}/{project_slug}"

[tool.maturin]
module-name = "{py_package}"
python-source = "python"
strip = true
'''
    _write(os.path.join(bindings_dir, "pyproject.toml"), pyproject_content)

    # src/lib.rs
    lib_rs = f'''use pyo3::prelude::*;

/// A simple test function.
#[pyfunction]
fn hello() -> String {{
    "Hello from {project_slug}!".to_string()
}}

/// Python module for {project_name}.
#[pymodule]
fn {py_package}(m: &Bound<'_, PyModule>) -> PyResult<()> {{
    m.add_function(wrap_pyfunction!(hello, m)?)?;
    Ok(())
}}

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn test_hello() {{
        assert_eq!(hello(), "Hello from {project_slug}!");
    }}
}}
'''
    _write(os.path.join(bindings_dir, "src", "lib.rs"), lib_rs)

    # python/{package}/__init__.py
    init_py = f'''from .{py_package} import *

__all__ = ["hello"]
'''
    _write(os.path.join(bindings_dir, "python", py_package, "__init__.py"), init_py)

    # tests/test_basic.py
    test_py = f'''import {py_package}


def test_hello():
    result = {py_package}.hello()
    assert result == "Hello from {project_slug}!"
'''
    _write(os.path.join(bindings_dir, "tests", "test_basic.py"), test_py)

    # README.md
    readme = f'''# {project_slug}

Python bindings for {project_name}, built with [PyO3](https://pyo3.rs) and [Maturin](https://www.maturin.rs).

## Development

```bash
# Install in development mode
maturin develop

# Run tests
pytest tests/

# Build a release wheel
maturin build --release
```
'''
    _write(os.path.join(bindings_dir, "README.md"), readme)

    # Add bindings exclusion to workspace Cargo.toml
    cargo_path = os.path.join(project_dir, "Cargo.toml")
    with open(cargo_path, "r") as f:
        cargo_content = f.read()
    # Add exclude after members line
    if 'exclude' not in cargo_content:
        cargo_content = cargo_content.replace(
            'resolver = "2"',
            'exclude = ["bindings/*"]\nresolver = "2"',
        )
        with open(cargo_path, "w") as f:
            f.write(cargo_content)

    # Add maturin to flox manifest if .flox exists
    flox_manifest = os.path.join(project_dir, ".flox", "env", "manifest.toml")
    if os.path.isfile(flox_manifest):
        with open(flox_manifest, "r") as f:
            manifest = f.read()
        if "maturin" not in manifest:
            manifest = manifest.replace(
                'mdbook.pkg-path = "mdbook"',
                'mdbook.pkg-path = "mdbook"\nmaturin.pkg-path = "maturin"',
            )
            with open(flox_manifest, "w") as f:
                f.write(manifest)
            print("  Added maturin to Flox manifest")

    print(f"  Created Python bindings at bindings/{project_slug}-py/")
    print(f"  Module name: {py_package}")
    print("  Build with: cd bindings/" + f"{project_slug}-py && maturin develop")


def _write(path, content):
    """Write content to a file, creating parent directories as needed."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)
