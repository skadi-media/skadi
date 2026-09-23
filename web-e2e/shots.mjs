// Seed the running harness with a movie (+ enable domains), then screenshot every
// screen — including the interactions (drawer, add search) — for visual review.
// Run: SKADI_E2E_WORKDIR=web-e2e/.tmp-e2e node web-e2e/shots.mjs  (harness on :8191)
import { chromium } from "@playwright/test";
import { mkdirSync } from "node:fs";
import * as path from "node:path";

const BASE = process.env.SKADI_URL || "http://127.0.0.1:8191";
const API = `${BASE}/api/v1`;
const WORKDIR =
  process.env.SKADI_E2E_WORKDIR || path.resolve("web-e2e/.tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");
const OUT = "web-e2e/shots";
mkdirSync(OUT, { recursive: true });

const J = { "content-type": "application/json" };
const post = (p, body) =>
  fetch(`${API}${p}`, { method: "POST", headers: J, body: JSON.stringify(body) });
const put = (p, body) =>
  fetch(`${API}${p}`, { method: "PUT", headers: J, body: JSON.stringify(body) });

async function seed() {
  for (const d of ["movies", "audiobooks"]) {
    await put(`/domains/${d}`, { enabled: true });
  }
  const prof = await (await post("/settings/profiles", { name: "1080p", cutoff: "Bluray-1080p" })).json();
  // `root_folders` was removed by SKADI-T-0302; the single root is a config key.
  await put("/config/library.root", { value: LIBRARY });
  await post("/movies", { tmdb_id: 603, profile: prof.id, root_folder: LIBRARY, search: false });
  // An audiobook (mock Audnexus → "Project Hail Mary"). Best-effort.
  await post("/authors", { asin: "B017V4IM1G" }).catch(() => {});
  await post("/books", { asin: "B017V4IM1G", root: LIBRARY, search: false }).catch(() => {});
}

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
const page = await ctx.newPage();
const errs = [];
page.on("console", (m) => m.type() === "error" && errs.push(m.text()));

await seed();
await page.waitForTimeout(1500);

const shoot = async (name, route, prep) => {
  await page.goto(BASE + route, { waitUntil: "networkidle", timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(1200);
  if (prep) await prep().catch((e) => console.log(`  prep ${name}: ${e.message}`));
  await page.screenshot({ path: `${OUT}/x-${name}.png`, fullPage: true });
  console.log(`captured x-${name} (${route})`);
};

await shoot("overview", "/");
await shoot("movies", "/movies");
await shoot("drawer", "/movies", async () => {
  await page.locator(".poster-tile").first().click();
  await page.waitForTimeout(500);
  await page.getByRole("button", { name: /Search releases|upgrade/ }).click();
  await page.waitForTimeout(1500);
});
await shoot("add", "/add", async () => {
  await page.locator(".add-search-input").fill("matrix");
  await page.locator(".add-search-input").press("Enter");
  await page.waitForTimeout(1500);
});
await shoot("audiobooks", "/audiobooks");
await shoot("activity", "/activity");
await shoot("downloads", "/downloaders");
await shoot("indexers", "/indexers");
await shoot("indexers-catalog", "/indexers", async () => {
  await page.getByRole("button", { name: /Add tracker \(native\)/ }).click();
  await page.waitForTimeout(1200);
});
await shoot("config", "/config");
await shoot("setup", "/setup");

if (errs.length) console.log("\nconsole errors:\n" + [...new Set(errs)].join("\n"));
await browser.close();
console.log("DONE");
