import { chromium } from "@playwright/test";
const BASE = process.env.SKADI_URL || "http://127.0.0.1:8090";
const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: 1440, height: 1200 },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
await page
  .goto(BASE + "/activity", { waitUntil: "networkidle", timeout: 25000 })
  .catch(() => {});
// Let the 2.5s poll land at least one trace/activity cycle.
await page.waitForTimeout(3500);
await page.screenshot({ path: "web-e2e/shots/live-activity.png", fullPage: false });
console.log("captured live-activity");
await browser.close();
