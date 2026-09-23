import { defineConfig, devices } from "@playwright/test";
import * as path from "path";

// The harness daemon serves the embedded UI + mock-provider API on this port,
// using WORKDIR for its DB + library + the fixture .mkv. The spec reads the same
// WORKDIR (via process.env) to point a root folder at <WORKDIR>/library.
const PORT = process.env.SKADI_E2E_PORT || "8191";
const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, ".tmp-e2e");
const BASE_URL = `http://127.0.0.1:${PORT}`;

// Make WORKDIR visible to the spec process too (webServer.env only covers the
// server).
process.env.SKADI_E2E_WORKDIR = WORKDIR;

export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  workers: 1,
  reporter: process.env.CI ? "line" : "list",
  timeout: 60_000,
  expect: { timeout: 15_000 },
  use: {
    baseURL: BASE_URL,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    // Builds the UI (trunk) then runs the harness with the UI embedded.
    command: "bash run-harness.sh",
    cwd: __dirname,
    url: BASE_URL,
    // First run compiles the web (wasm) + the harness — allow plenty of time.
    timeout: 300_000,
    reuseExistingServer: !process.env.CI,
    stdout: "pipe",
    stderr: "pipe",
    env: { SKADI_E2E_PORT: PORT, SKADI_E2E_WORKDIR: WORKDIR },
  },
});
