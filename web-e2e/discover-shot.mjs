import { chromium } from "@playwright/test";
const BASE = process.env.SKADI_URL || "http://127.0.0.1:8090";
const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: 1440, height: 1150 },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
await page
  .goto(BASE + "/audiobooks/discover", { waitUntil: "networkidle", timeout: 25000 })
  .catch(() => {});
await page.waitForTimeout(2500);
await page.screenshot({ path: "web-e2e/shots/live-discover.png", fullPage: false });
console.log("captured live-discover");
await browser.close();
