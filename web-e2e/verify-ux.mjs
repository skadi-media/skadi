import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const browser = await chromium.launch();
const desk = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
const dp = await desk.newPage();
for (const [name, path] of [
  ["overview", "/"],        // indexer health rollup + storage bar color
  ["config", "/config"],    // TV toggle colored + "TV" casing
  ["activity", "/activity"],// stage bar + no UUID titles
  ["audiobooks", "/audiobooks"], // dot-only-non-owned, square covers, Listen link
]) {
  await dp.goto(BASE + path, { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
  await dp.waitForTimeout(1500);
  await dp.screenshot({ path: `web-e2e/shots/tour2/${name}.png` });
  console.log("desk:", name);
}
// Phone-first Listen page (split paths) + expand the app path
const phone = await browser.newContext({ viewport: { width: 412, height: 915 }, deviceScaleFactor: 2 });
const pp = await phone.newPage();
await pp.goto(BASE + "/listen", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await pp.waitForTimeout(1200);
await pp.locator("text=Set up").first().click().catch(() => {});
await pp.waitForTimeout(2000);
await pp.screenshot({ path: "web-e2e/shots/tour2/listen.png" });
console.log("phone: listen");
await browser.close();
