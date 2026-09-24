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
// Which kind to exercise. Both matter: they take different probe paths on the
// server and land on different import pages.
const KIND = process.env.KIND || "audiobook";
const FIXTURE = process.env.FIXTURE || (KIND === "audiobook"
  ? "../crates/skadi-api/tests/fixtures/tone.wav"
  : "../crates/skadi-media-probe/tests/fixtures/tiny.mp4");

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
await page.selectOption("select", KIND).catch(e => console.log("kind select: " + e.message.slice(0, 80)));
await page.waitForTimeout(400);
const note = await page.locator(".pending").count();
console.log("asin note:" + (KIND === "audiobook"
  ? (note ? " shown" : " MISSING")
  : (note ? " WRONGLY SHOWN for " + KIND : " correctly absent for " + KIND)));

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

  // "No error" is not "it found the file". Wait for the scan to produce a row
  // naming what was uploaded — that is the actual end of the hand-off.
  // Look for the uploaded file's own name. The three import pages render
  // results differently — the TV one groups by show rather than using a plain
  // table — so counting `tbody tr` reported zero for a scan that had in fact
  // found the episode. Match on the name, and give the scan long enough: it
  // resolves metadata over the network.
  const stem = FIXTURE.split("/").pop().replace(/\.[^.]+$/, "");
  const needle = stem.split(/[ .]/)[0];
  let found = "";
  for (let i = 0; i < 60; i++) {
    await page.waitForTimeout(500);
    const text = await page.locator("body").first().innerText().catch(() => "");
    if (text.includes(needle)) { found = "yes"; break; }
    if (/no .*(candidates|files)|nothing to import/i.test(text)) { found = "scan found nothing"; break; }
  }
  console.log(`scanned:  ${found || "nothing matching " + JSON.stringify(needle) + " within 30s"}`);
} else {
  console.log("hand-off: MISSING");
  const err = await page.locator(".upload-job .bad").first().textContent().catch(() => "");
  if (err) console.log("error:    " + err);
}

await page.screenshot({ path: "shots/upload-verify.png", fullPage: true });
console.log("errors:   " + JSON.stringify(errors));
await browser.close();
