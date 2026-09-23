// Walk the skadi web nav tree and screenshot each page for review.
// Run: node web-e2e/walk.mjs   (against the live daemon on :8090)
import { chromium } from "@playwright/test";
import { mkdirSync } from "node:fs";

const BASE = process.env.SKADI_URL || "http://127.0.0.1:8090";
const BOOK = "132f565a-cca4-4e0e-a851-4cc4988c6ede";
const OUT = "web-e2e/shots";
mkdirSync(OUT, { recursive: true });

const pages = [
  ["00-dashboard", "/"],
  ["01-activity", "/activity"],
  ["02-movies", "/movies"],
  ["03-movies-config", "/movies/config"],
  ["04-movies-import", "/movies/import"],
  ["05-audiobooks", "/audiobooks"],
  ["06-audiobook-detail", `/audiobooks/${BOOK}`],
  ["07-audiobooks-config", "/audiobooks/config"],
  ["08-audiobooks-import", "/audiobooks/import"],
  ["09-indexers", "/indexers"],
  ["10-downloaders", "/downloaders"],
  ["11-config", "/config"],
];

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
const page = await ctx.newPage();
const errors = [];
page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });

for (const [name, route] of pages) {
  try {
    await page.goto(BASE + route, { waitUntil: "networkidle", timeout: 20000 });
  } catch (e) {
    console.log(`! ${route} nav: ${e.message}`);
  }
  // Let the wasm app render + any data fetch settle.
  await page.waitForTimeout(1800);
  await page.screenshot({ path: `${OUT}/${name}.png`, fullPage: true });
  console.log(`captured ${name}  (${route})`);
}

if (errors.length) console.log("\nconsole errors:\n" + [...new Set(errors)].join("\n"));
await browser.close();
console.log("DONE");
