import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const browser = await chromium.launch();
const page = await (await browser.newContext({ viewport: { width: 1440, height: 900 } })).newPage();
const errors = [];
page.on("console", m => { if (m.type() === "error") errors.push(m.text().slice(0, 160)); });
await page.goto(BASE + "/", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(1500);
for (const name of ["Movies", "TV", "Audiobooks"]) {
  await page.locator(`nav a:has-text("${name}")`).first().click().catch(e => console.log(`${name}: click failed ${e.message.slice(0,80)}`));
  await page.waitForTimeout(1200);
  console.log(`${name} -> url=${page.url()} h2=${await page.locator("h2").first().textContent().catch(() => "?")}`);
}
await page.goto(BASE + "/", { waitUntil: "networkidle" }).catch(() => {});
await page.waitForTimeout(1000);
// Dashboard Television card link
const tvCard = page.locator('a:has-text("Television")').first();
if (await tvCard.count()) {
  await tvCard.click();
  await page.waitForTimeout(1000);
  console.log("dashboard Television card -> url=" + page.url());
} else { console.log("no dashboard Television link found"); }
console.log("console errors:", JSON.stringify(errors));
await browser.close();
