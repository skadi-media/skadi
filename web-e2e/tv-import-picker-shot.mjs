import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 2 });
const page = await ctx.newPage();
await page.goto(BASE + "/tv/import", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(1500);
await page.locator("input.path-field").first().fill("/mnt/storage/television");
await page.getByRole("button", { name: "Scan" }).first().click();
// Real library scan ~20s; give it room.
await page.waitForTimeout(28000);
// Open the picker on the first show card and search a known mis-match case.
await page.getByRole("button", { name: /Find show/ }).first().click();
await page.waitForTimeout(400);
const pick = page.locator(".import-picker input.path-field").first();
await pick.fill("lexx");
await page.locator(".import-picker-search button").first().click();
await page.waitForTimeout(3500);
await page.screenshot({ path: "web-e2e/shots/live-tv-import-picker.png" });
console.log("captured picker");
await browser.close();
