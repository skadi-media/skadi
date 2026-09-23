import { chromium } from "@playwright/test";
const BASE = "http://127.0.0.1:8090";
const browser = await chromium.launch();
const page = await (await browser.newContext({ viewport: { width: 1440, height: 1000 } })).newPage();
const errors = [];
page.on("console", m => { if (m.type() === "error") errors.push(m.text().slice(0, 200)); });
await page.goto(BASE + "/tv/3e943105-51b5-4109-a2cd-69b26457d59e", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await page.waitForTimeout(2500);
const link = page.locator("a.btn-link", { hasText: "← Library" }).first();
console.log("link count:", await page.locator("a.btn-link").count());
console.log("visible:", await link.isVisible().catch(() => "err"));
console.log("href:", await link.getAttribute("href").catch(() => "err"));
const box = await link.boundingBox().catch(() => null);
console.log("bbox:", JSON.stringify(box));
// What element actually sits at that point?
if (box) {
  const at = await page.evaluate(([x, y]) => {
    const el = document.elementFromPoint(x, y);
    return el ? el.tagName + "." + el.className : "none";
  }, [box.x + box.width / 2, box.y + box.height / 2]);
  console.log("element at click point:", at);
}
await link.click({ timeout: 5000 }).catch(e => console.log("click err:", e.message.slice(0, 120)));
await page.waitForTimeout(1200);
console.log("url after:", page.url());
console.log("console errors:", JSON.stringify(errors));
await browser.close();
