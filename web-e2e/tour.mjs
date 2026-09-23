import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const MID = "79289020-c02a-40a8-80ce-1cd09188077f";
const SID = "3e943105-51b5-4109-a2cd-69b26457d59e";
const BID = "d127e315-deee-4f26-a1cf-07560c02a854";
const FID = "ce435f08-43e9-4f6e-a53f-8fff680d733a";
const browser = await chromium.launch();

// Desktop viewport for the library/admin views.
const desk = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
const dp = await desk.newPage();
const deskViews = [
  ["overview", "/"],
  ["movies", "/movies"],
  ["tv", "/tv"],
  ["audiobooks", "/audiobooks"],
  ["activity", "/activity"],
  ["add", "/add"],
  ["indexers", "/indexers"],
  ["downloaders", "/downloaders"],
  ["config", "/config"],
  ["naming", "/naming"],
  ["movie-detail", `/movies/${MID}`],
  ["series-detail", `/tv/${SID}`],
  ["book-detail", `/audiobooks/${BID}`],
];
for (const [name, path] of deskViews) {
  await dp.goto(BASE + path, { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
  await dp.waitForTimeout(1400);
  await dp.screenshot({ path: `web-e2e/shots/tour/web-${name}.png` });
  console.log("desk:", name);
}

// Phone viewport for the phone-first player surface.
const phone = await browser.newContext({ viewport: { width: 412, height: 915 }, deviceScaleFactor: 2 });
const pp = await phone.newPage();
await pp.goto(BASE + "/listen", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await pp.waitForTimeout(1200);
await pp.screenshot({ path: "web-e2e/shots/tour/web-listen.png" });
console.log("phone: listen");
// Pair card open
await pp.locator("text=Pair your phone").click().catch(() => {});
await pp.waitForTimeout(1500);
await pp.screenshot({ path: "web-e2e/shots/tour/web-pair.png" });
console.log("phone: pair");
// Player (download + play)
await pp.goto(`${BASE}/listen/${BID}/${FID}`, { waitUntil: "domcontentloaded", timeout: 25000 }).catch(() => {});
await pp.waitForFunction(() => document.querySelector("audio")?.src?.startsWith("blob:"), { timeout: 180000 }).catch(() => {});
await pp.waitForTimeout(1500);
await pp.locator(".player-btn-sm", { hasText: "Chapters" }).click().catch(() => {});
await pp.waitForTimeout(800);
await pp.screenshot({ path: "web-e2e/shots/tour/web-player.png" });
console.log("phone: player");
await browser.close();
