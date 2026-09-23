import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const browser = await chromium.launch();
const page = await (await browser.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 2 })).newPage();
await page.goto(BASE + "/tv", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(2500);
await page.screenshot({ path: "web-e2e/shots/tv-lib.png" });
console.log("captured");
await browser.close();
