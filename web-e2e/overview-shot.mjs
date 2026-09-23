import { chromium } from "@playwright/test";
const b = await chromium.launch();
const c = await b.newContext({ viewport: { width: 1440, height: 1100 }, deviceScaleFactor: 2 });
const p = await c.newPage();
await p.goto("http://127.0.0.1:8090/", { waitUntil: "networkidle", timeout: 25000 }).catch(()=>{});
await p.waitForTimeout(3000);
await p.screenshot({ path: "web-e2e/shots/live-overview.png", fullPage: false });
console.log("captured overview");
await b.close();
