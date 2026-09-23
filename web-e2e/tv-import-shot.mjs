import { chromium } from "@playwright/test";
const BASE = process.env.SKADI_URL || "http://127.0.0.1:8090";
const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: 1440, height: 1000 },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
// TV page first (to show the Import link on the header), then the import page.
await page.goto(BASE + "/tv", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(2000);
await page.screenshot({ path: "web-e2e/shots/live-tv-header.png", fullPage: false });
await page
  .goto(BASE + "/tv/import", { waitUntil: "networkidle", timeout: 25000 })
  .catch(() => {});
await page.waitForTimeout(2000);
await page.screenshot({ path: "web-e2e/shots/live-tv-import.png", fullPage: false });
console.log("captured tv-header + tv-import");
await browser.close();
