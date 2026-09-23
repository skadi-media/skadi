import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const BID = "d127e315-deee-4f26-a1cf-07560c02a854", FID = "ce435f08-43e9-4f6e-a53f-8fff680d733a";
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 412, height: 915 } });
const page = await ctx.newPage();
page.on("console", m => { if (m.type() === "error") console.log("ERR:", m.text().slice(0, 200)); });

// Online: play once → save-on-listen writes OPFS.
await page.goto(`${BASE}/listen/${BID}/${FID}`, { waitUntil: "networkidle", timeout: 30000 }).catch(() => {});
await page.waitForFunction(() => document.querySelector("audio")?.src?.startsWith("blob:"), { timeout: 180000 });
await page.waitForTimeout(8000); // OPFS write of 360MB
console.log("online: blob loaded + save window elapsed");

// Kill the network. All further navigation is CLIENT-SIDE (SPA routing) —
// cold-start offline needs the T-0333 service worker, not this task.
await ctx.setOffline(true);
await page.evaluate(() => document.querySelector('a[href="/listen"]').click());
await page.waitForTimeout(1500);
const rows = await page.locator(".shelf-row").count();
console.log("OFFLINE shelf rows:", rows);
await page.evaluate(() => document.querySelector(".shelf-main").click());
await page.waitForFunction(() => document.querySelector("audio")?.src?.startsWith("blob:"), { timeout: 20000 })
  .then(() => console.log("OFFLINE: player loaded audio from OPFS"))
  .catch(() => console.log("OFFLINE: no blob — FAIL"));
await page.waitForTimeout(800);
await page.locator(".player-play").click().catch(() => {});
await page.waitForTimeout(3000);
const st = await page.evaluate(() => {
  const a = document.querySelector("audio");
  return { time: a?.currentTime?.toFixed(1), paused: a?.paused,
           title: document.querySelector(".player-title")?.textContent };
});
await page.locator(".player-btn-sm", { hasText: "Chapters" }).click().catch(() => {});
await page.waitForTimeout(500);
st.chapters = await page.locator(".player-ch").count();
console.log("OFFLINE state:", JSON.stringify(st));
await page.screenshot({ path: "web-e2e/shots/offline-player.png" });
await browser.close();
