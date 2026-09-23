import { chromium } from "@playwright/test";
const svg = (s) => `<!DOCTYPE html><html><body style="margin:0">
<div style="width:${s}px;height:${s}px;background:#0b0e13;display:flex;align-items:center;justify-content:center;border-radius:0">
<span style="font-family:-apple-system,sans-serif;font-size:${s * 0.62}px;color:#7fb2ff;transform:translateY(-${s * 0.04}px)">&#10059;</span>
</div></body></html>`;
const browser = await chromium.launch();
for (const s of [192, 512]) {
  const page = await (await browser.newContext({ viewport: { width: s, height: s } })).newPage();
  await page.setContent(svg(s));
  await page.waitForTimeout(300);
  await page.screenshot({ path: `../crates/skadi-web/icon-${s}.png`, clip: { x: 0, y: 0, width: s, height: s } });
  console.log(`icon-${s}.png`);
}
await browser.close();
