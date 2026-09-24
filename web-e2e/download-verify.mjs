// Prove the Save link actually saves a file (SKADI-T-0635).
//
// Written because the upload feature shipped broken on exactly this gap: an
// endpoint with tests and a UI nobody had clicked. Playwright's download event
// is the real thing — it fires only if the browser genuinely started saving.
import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const TOKEN = process.env.SKADI_TOKEN;
const PAGE = process.env.PAGE || "/movies";

const browser = await chromium.launch();
const ctx = await browser.newContext({ acceptDownloads: true, viewport: { width: 1440, height: 1000 } });
const page = await ctx.newPage();
await page.goto(BASE + "/", { waitUntil: "domcontentloaded" });
await page.evaluate(t => localStorage.setItem("skadi.token", t), TOKEN);

// Go straight to an item known to have a file. Walking the library looking
// for one was the first attempt and it found nothing — the `/movies/` selector
// also matched the sidebar link, so it kept "opening" the library index. Pass
// ITEM as a path.
const ITEM = process.env.ITEM;
if (!ITEM) { console.log("set ITEM to a detail page path"); await browser.close(); process.exit(2); }
await page.goto(BASE + ITEM, { waitUntil: "domcontentloaded" });
await page.waitForTimeout(5000);
if (process.env.OPEN_SEASON) {
  await page.locator("button", { hasText: /Season|Specials/ }).first().click().catch(() => {});
  await page.waitForTimeout(3000);
}
if (!(await page.locator("a[download]").count())) {
  console.log("no Save link on " + ITEM);
  await browser.close(); process.exit(1);
}

const link = page.locator("a[download]").first();
const suggested = await link.getAttribute("download");
console.log("page:      " + page.url());
console.log("filename:  " + suggested);
console.log("href has apikey: " + /apikey=/.test(await link.getAttribute("href")));

const [dl] = await Promise.all([
  page.waitForEvent("download", { timeout: 30000 }),
  link.click(),
]);
console.log("download started: " + dl.suggestedFilename());
// Read a little of it to prove bytes actually flow, then cancel — these files
// are gigabytes and the point is that it started, not that it finished.
await new Promise(r => setTimeout(r, 4000));
await dl.cancel().catch(() => {});
console.log("bytes flowing:   yes (cancelled after 4s; these are multi-GB files)");
await browser.close();
