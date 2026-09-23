import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const BID = "d127e315-deee-4f26-a1cf-07560c02a854"; // "14" (Peter Clines)
const FID = "ce435f08-43e9-4f6e-a53f-8fff680d733a";
const browser = await chromium.launch();
// Pixel-ish phone viewport.
const page = await (await browser.newContext({ viewport: { width: 412, height: 915 }, deviceScaleFactor: 2 })).newPage();
const errors = [];
page.on("console", m => { if (m.type() === "error") errors.push(m.text().slice(0, 150)); });

// Entry point: the book detail must show ▶ Listen.
await page.goto(`${BASE}/audiobooks/${BID}`, { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(1500);
const listen = page.locator(".listen-link").first();
console.log("listen link present:", await listen.count() > 0);
await listen.click().catch(e => console.log("listen click failed:", e.message.slice(0, 80)));
await page.waitForTimeout(1000);
console.log("player url:", page.url());

// Wait for the 360MB download (localhost — fast) then play.
await page.waitForFunction(() => {
  const a = document.querySelector("audio");
  return a && a.src.startsWith("blob:");
}, { timeout: 120000 }).catch(() => console.log("download did not finish"));
await page.waitForTimeout(1500);
await page.locator(".player-play").click().catch(e => console.log("play failed:", e.message.slice(0, 80)));
await page.waitForTimeout(4000);
const state = await page.evaluate(() => {
  const a = document.querySelector("audio");
  return { time: a?.currentTime, dur: a?.duration, paused: a?.paused, rate: a?.playbackRate };
});
console.log("audio state:", JSON.stringify(state));
// Open the chapter drawer for the shot.
await page.locator(".player-btn-sm", { hasText: "Chapters" }).click().catch(() => {});
await page.waitForTimeout(800);
console.log("chapter rows:", await page.locator(".player-ch").count());
await page.screenshot({ path: "web-e2e/shots/player.png", fullPage: false });
console.log("console errors:", JSON.stringify(errors.slice(0, 3)));
await browser.close();
