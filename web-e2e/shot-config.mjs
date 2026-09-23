import { chromium } from "@playwright/test";
const b = await chromium.launch();
const p = await (await b.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 })).newPage();
await p.goto("http://127.0.0.1:8090/config", { waitUntil: "networkidle", timeout: 25000 }).catch(() => {});
await p.waitForTimeout(1500);
await p.screenshot({ path: "web-e2e/shots/tour2/config.png" });
console.log("config reshot");
await b.close();
