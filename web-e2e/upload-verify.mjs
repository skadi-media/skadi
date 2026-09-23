// Drive the upload page in a real browser (SKADI-I-0062).
//
// The server half was proven with curl and 28 integration tests, and the
// client half with unit tests of its pure helpers. Neither of those opens the
// page. This does: pick a file, watch it go, and follow the hand-off — which
// is the only way to know the feature exists rather than merely compiles.
import { chromium } from "@playwright/test";
import { readFileSync } from "node:fs";

const BASE = "http://127.0.0.1:8090";
const TOKEN = process.env.SKADI_TOKEN;
const FIXTURE = "../crates/skadi-api/tests/fixtures/tone.wav";

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
const page = await ctx.newPage();
const errors = [];
page.on("console", m => { if (m.type() === "error") errors.push(m.text().slice(0, 200)); });
page.on("pageerror", e => errors.push("pageerror: " + String(e).slice(0, 200)));
// The status code alone does not say which call failed or why.
page.on("response", async r => {
  if (r.status() >= 400 && r.url().includes("/api/")) {
    const body = await r.text().catch(() => "");
    console.log(`  HTTP ${r.status()} ${r.request().method()} ${r.url().replace(BASE, "")} :: ${body.slice(0, 200)}`);
  }
});

// Sign in the way the app does: the token lives in localStorage.
await page.goto(BASE + "/", { waitUntil: "domcontentloaded" });
await page.evaluate(t => localStorage.setItem("skadi.token", t), TOKEN);

console.log("token set: " + await page.evaluate(() => (localStorage.getItem("skadi.token") || "").length) + " chars");
await page.goto(BASE + "/upload", { waitUntil: "networkidle", timeout: 30000 });
await page.waitForTimeout(1500);

console.log("url:      " + page.url());
console.log("heading:  " + await page.locator("h2").first().textContent().catch(() => "(none)"));
console.log("strip:    " + (await page.locator(".subnav-link").allTextContents()).join(" | "));

// Choose the kind, then the file.
await page.selectOption("select", "audiobook").catch(e => console.log("kind select: " + e.message.slice(0, 80)));
await page.waitForTimeout(400);
console.log("asin note:" + (await page.locator(".pending").count() ? " shown" : " MISSING"));

await page.setInputFiles('input[type="file"]', FIXTURE);
console.log("picked:   " + FIXTURE);

// Wait for the job to reach a terminal state.
let state = "";
for (let i = 0; i < 60; i++) {
  await page.waitForTimeout(500);
  state = (await page.locator(".upload-job .badge").first().textContent().catch(() => "")) || "";
  if (state === "done" || state === "failed") break;
}
console.log("state:    " + state);
console.log("progress: " + await page.locator(".upload-fill").first().getAttribute("style").catch(() => "?"));

const link = page.locator('.upload-job a:has-text("Identify it now")');
if (await link.count()) {
  const href = await link.first().getAttribute("href");
  console.log("hand-off: " + href);
  await link.first().click();
  await page.waitForTimeout(2500);
  console.log("landed:   " + page.url());
  console.log("import h2:" + await page.locator("h2").first().textContent().catch(() => "?"));
  const pathField = await page.locator('input.path-field, input[type="text"]').first().inputValue().catch(() => "");
  console.log("path box: " + (pathField || "(empty)"));
} else {
  console.log("hand-off: MISSING");
  const err = await page.locator(".upload-job .bad").first().textContent().catch(() => "");
  if (err) console.log("error:    " + err);
}

await page.screenshot({ path: "shots/upload-verify.png", fullPage: true });
console.log("errors:   " + JSON.stringify(errors));
await browser.close();
