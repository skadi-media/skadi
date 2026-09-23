import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
const page = await ctx.newPage();
await page.goto(BASE + "/tv/import", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(1500);
await page.locator("input.path-field").first().fill("/mnt/storage/television");
await page.getByRole("button", { name: "Scan" }).first().click();
// Wait for the review header (scan + first match round) rather than a blind sleep.
await page.waitForSelector("text=/shows ·|shows \\(/", { timeout: 150000 }).catch(() => {});
await page.waitForTimeout(12000);
await page.screenshot({ path: "web-e2e/shots/live-tv-import-final.png" });
console.log("captured final");
await browser.close();
