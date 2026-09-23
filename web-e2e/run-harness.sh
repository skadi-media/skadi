#!/usr/bin/env bash
# Build the web UI then launch the mock-provider E2E harness with it embedded.
# Invoked by Playwright's `webServer` (SKADI-T-0120).
set -euo pipefail

# Repo root (this script lives in web-e2e/).
cd "$(dirname "$0")/.."

export SKADI_E2E_PORT="${SKADI_E2E_PORT:-8191}"
export SKADI_E2E_WORKDIR="${SKADI_E2E_WORKDIR:-$(pwd)/web-e2e/.tmp-e2e}"

# Fresh state each run.
rm -rf "$SKADI_E2E_WORKDIR"

# 1) Build the Leptos UI into crates/skadi-web/dist (embedded by --features embed-ui).
( cd crates/skadi-web && trunk build )

# 2) Serve it via the harness (mock indexer/downloader/TMDB).
exec cargo run -p skadi-e2e-harness --features embed-ui
