import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const URL = `${BASE}/listen/d127e315-deee-4f26-a1cf-07560c02a854/ce435f08-43e9-4f6e-a53f-8fff680d733a`;
const browser = await chromium.launch();
const page = await (await browser.newContext({ viewport: { width: 412, height: 915 } })).newPage();
page.on("console", m => { if (m.type() === "error") console.log("CONSOLE:", m.text().slice(0, 600)); });
page.on("pageerror", e => console.log("PAGEERROR:", String(e).slice(0, 600)));
// DIRECT navigation — no detail page involved.
await page.goto(URL, { waitUntil: "networkidle", timeout: 30000 }).catch(() => {});
await page.waitForFunction(() => {
  const a = document.querySelector("audio");
  return a && a.src.startsWith("blob:");
}, { timeout: 180000 }).then(() => console.log("blob loaded")).catch(() => console.log("no blob after 180s"));
await page.waitForTimeout(2000);
await page.locator(".player-play").click().catch(e => console.log("play click failed:", e.message.slice(0,80)));
await page.waitForTimeout(4000);
console.log("state:", JSON.stringify(await page.evaluate(() => {
  const a = document.querySelector("audio");
  return { time: a?.currentTime?.toFixed(1), dur: a?.duration?.toFixed(0), paused: a?.paused };
})));
await browser.close();
