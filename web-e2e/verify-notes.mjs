import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const browser = await chromium.launch();
const page = await (await browser.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 2 })).newPage();

// 1) Back-button: open a series detail DIRECTLY, click "← Library".
await page.goto(BASE + "/tv/3e943105-51b5-4109-a2cd-69b26457d59e", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(2000);
const before = page.url();
await page.locator("text=← Library").first().click().catch(e => console.log("back click failed:", e.message.slice(0,80)));
await page.waitForTimeout(1200);
const after = page.url();
console.log("back-button: " + before + " -> " + after + "  PASS=" + (after.endsWith("/tv")));

// 2) Extras: scan, untick hide, read Black Sails header.
await page.goto(BASE + "/tv/import", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(1200);
await page.locator("input.path-field").first().fill("/mnt/storage/television");
await page.getByRole("button", { name: "Scan" }).first().click();
await page.waitForSelector("text=/shows ·|shows \\(/", { timeout: 180000 }).catch(() => {});
await page.waitForTimeout(15000);
await page.locator(".import-hide-toggle input").first().click().catch(() => {});
await page.waitForTimeout(8000);
const bs = page.locator(".import-show", { hasText: "Black Sails" }).first();
console.log("black sails header:", JSON.stringify(await bs.locator(".season-count").first().textContent().catch(() => "(not found)")));
await bs.screenshot({ path: "web-e2e/shots/verify-extras.png" }).catch(() => {});
await browser.close();
