import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const BID = "d127e315-deee-4f26-a1cf-07560c02a854", FID = "ce435f08-43e9-4f6e-a53f-8fff680d733a";
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 412, height: 915 } });
const page = await ctx.newPage();
page.on("console", m => { if (m.type() === "error") console.log("ERR:", m.text().slice(0, 200)); });

// 1) Shell pieces served?
for (const p of ["/manifest.webmanifest", "/sw.js", "/icon-192.png"]) {
  const r = await page.request.get(BASE + p);
  console.log(p, "->", r.status());
}

// 2) Online: register SW, reload once so the SW caches shell assets, play the book.
await page.goto(`${BASE}/listen`, { waitUntil: "networkidle", timeout: 30000 });
await page.evaluate(() => navigator.serviceWorker.ready.then(() => true));
console.log("SW ready");
await page.reload({ waitUntil: "networkidle" }); // assets now fetched THROUGH the SW
await page.waitForTimeout(1000);
// Pair card (online):
await page.locator("text=Pair your phone").click();
await page.waitForSelector(".pair-qr svg", { timeout: 10000 }).then(() => console.log("QR rendered")).catch(() => console.log("QR FAIL"));
await page.screenshot({ path: "web-e2e/shots/pair-qr.png" });
// Play once → OPFS save.
await page.goto(`${BASE}/listen/${BID}/${FID}`, { waitUntil: "domcontentloaded" });
await page.waitForFunction(() => document.querySelector("audio")?.src?.startsWith("blob:"), { timeout: 180000 });
await page.waitForTimeout(8000);
console.log("online play + save window done");

// 3) AIRPLANE MODE: network dead, full cold reload.
await ctx.setOffline(true);
await page.reload({ waitUntil: "domcontentloaded", timeout: 30000 }).catch(() => {});
await page.waitForFunction(() => document.querySelector("audio")?.src?.startsWith("blob:"), { timeout: 30000 })
  .then(() => console.log("OFFLINE COLD RELOAD: app loaded via SW + audio from OPFS"))
  .catch(() => console.log("OFFLINE COLD RELOAD: FAIL"));
await page.locator(".player-play").click().catch(() => {});
await page.waitForTimeout(3000);
const st = await page.evaluate(() => {
  const a = document.querySelector("audio");
  return { time: a?.currentTime?.toFixed(1), paused: a?.paused,
           title: document.querySelector(".player-title")?.textContent };
});
console.log("OFFLINE state:", JSON.stringify(st));
await page.screenshot({ path: "web-e2e/shots/pwa-offline.png" });
await browser.close();
