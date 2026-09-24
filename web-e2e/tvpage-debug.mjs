import { chromium } from "@playwright/test";
const BASE="http://127.0.0.1:8090", TOKEN=process.env.SKADI_TOKEN, DIR=process.env.DIR;
const b=await chromium.launch(); const p=await (await b.newContext()).newPage();
p.on("response", async r => {
  const u = r.url().replace(BASE,"");
  if (u.includes("library-import")) console.log(`  <- ${r.status()} ${u} :: ${(await r.text().catch(()=>"")).slice(0,300)}`);
  else if (r.status()>=400 && u.includes("/api/")) console.log(`  HTTP ${r.status()} ${r.request().method()} ${u}`);
});
p.on("request", r => { if (r.url().includes("library-import")) console.log("  -> " + r.method() + " " + r.url().replace(BASE,"") + " " + (r.postData()||"").slice(0,200)); });
await p.goto(BASE+"/", {waitUntil:"domcontentloaded"});
await p.evaluate(t=>localStorage.setItem("skadi.token",t), TOKEN);
await p.goto(BASE+"/tv/import?path="+encodeURIComponent(DIR), {waitUntil:"domcontentloaded"});
await p.waitForTimeout(25000);
const txt = await p.locator("body").innerText();
console.log("---- page text ----");
console.log(txt.split("\n").filter(l=>l.trim()).slice(0,25).join("\n"));
await b.close();
