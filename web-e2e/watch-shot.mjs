import { chromium } from "@playwright/test";
const b = await chromium.launch();
const p = await (await b.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 2 })).newPage();
await p.goto("http://127.0.0.1:8090/audiobooks", { waitUntil: "networkidle", timeout: 30000 }).catch(() => {});
await p.waitForTimeout(1500);

// Switch to Series organize mode.
await p.getByRole("button", { name: "Series", exact: true }).click().catch((e) => console.log("series chip:", e.message));
await p.waitForTimeout(1500);
await p.screenshot({ path: "web-e2e/shots/watch/series-headers.png" });
console.log("series-headers shot (Watch buttons + N/M counts)");

// Report what the headers look like + watch button count.
const heads = await p.$$eval(".lib-cluster-head", (els) =>
  els.slice(0, 12).map((e) => e.innerText.replace(/\n/g, " ").trim()));
const watchBtns = await p.$$eval(".watch-btn", (els) => els.map((e) => e.innerText.trim()));
console.log("HEADERS:", JSON.stringify(heads));
console.log("WATCH BUTTONS:", JSON.stringify(watchBtns));

// Expand the Dungeon Crawler Carl cluster (owned #1-8) to show the merged grid.
const dcc = p.locator(".lib-cluster-head", { hasText: "Dungeon Crawler Carl" }).first();
if (await dcc.count()) {
  await dcc.click();
  await p.waitForTimeout(2000);
  await dcc.scrollIntoViewIfNeeded();
  await p.screenshot({ path: "web-e2e/shots/watch/dcc-expanded.png" });
  console.log("dcc-expanded shot");
}

// Find any series cluster that has missing tiles, expand it, shoot.
const clusters = await p.$$(".lib-cluster");
for (const c of clusters) {
  const head = await c.$(".lib-cluster-head");
  const label = (await head?.innerText())?.replace(/\n/g, " ").trim() || "";
  if (!/\/\d/.test(label)) continue; // only N/M (incomplete) series
  await head.click();
  await p.waitForTimeout(1500);
  const missing = await c.$$(".poster-tile.missing");
  if (missing.length > 0) {
    await c.scrollIntoViewIfNeeded();
    await p.waitForTimeout(400);
    await p.screenshot({ path: "web-e2e/shots/watch/missing-inline.png" });
    console.log(`missing-inline shot: "${label}" has ${missing.length} missing tiles`);
    break;
  }
  await head.click(); // collapse and continue
  await p.waitForTimeout(300);
}

await b.close();
