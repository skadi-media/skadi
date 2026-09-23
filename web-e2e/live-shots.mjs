// Screenshot the LIVE deploy (:8090) — the daemon injects the API token into the
// served index.html, so the browser auto-authenticates. No seeding (real data).
import { chromium } from "@playwright/test";
import { mkdirSync } from "node:fs";

const BASE = process.env.SKADI_URL || "http://127.0.0.1:8090";
const OUT = "web-e2e/shots";
const BOOK5 = "3f22633a-7d67-4531-906d-6786521b7f70"; // DCC "The Butcher's Masquerade"
mkdirSync(OUT, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: 1440, height: 980 },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
const errs = [];
page.on("console", (m) => m.type() === "error" && errs.push(m.text()));

async function shoot(name, route, prep) {
  await page
    .goto(BASE + route, { waitUntil: "networkidle", timeout: 25000 })
    .catch(() => {});
  await page.waitForTimeout(1500);
  if (prep) await prep().catch((e) => console.log(`  prep ${name}: ${e.message}`));
  await page.screenshot({ path: `${OUT}/live-${name}.png`, fullPage: true });
  console.log(`captured live-${name} (${route})`);
}

await shoot("overview", "/");

await shoot("audiobooks-series", "/audiobooks", async () => {
  await page.getByRole("button", { name: "Series" }).first().click();
  await page.waitForTimeout(600);
  await page.getByText(/Dungeon Crawler Carl/i).first().click();
  await page.waitForTimeout(700);
});

// Book detail — History collapsed by default now.
await shoot("book-detail", `/audiobooks/${BOOK5}`, async () => {
  await page.waitForTimeout(2000);
});

await page
  .goto(BASE + `/audiobooks/${BOOK5}`, { waitUntil: "networkidle", timeout: 25000 })
  .catch(() => {});
await page.waitForTimeout(2500);

// Clip of the collapsed header (▸ History · N).
const hpc = page.locator(".history-panel").first();
if (await hpc.count()) {
  await hpc
    .screenshot({ path: `${OUT}/live-history-collapsed.png` })
    .then(() => console.log("captured live-history-collapsed"))
    .catch((e) => console.log("  collapsed shot: " + e.message));
}

// Click the header to expand, then clip the open panel.
await page.locator(".hist-head").first().click().catch(() => {});
await page.waitForTimeout(900);
const hpo = page.locator(".history-panel").first();
if (await hpo.count()) {
  await hpo
    .screenshot({ path: `${OUT}/live-history-open.png` })
    .then(() => console.log("captured live-history-open"))
    .catch((e) => console.log("  open shot: " + e.message));
}

if (errs.length) console.log("console errors:", errs.slice(0, 6));
await browser.close();
