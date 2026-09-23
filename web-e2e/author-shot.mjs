import { chromium } from "@playwright/test";
const b = await chromium.launch();
const p = await (await b.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 2 })).newPage();
await p.goto("http://127.0.0.1:8090/audiobooks", { waitUntil: "networkidle", timeout: 30000 }).catch(() => {});
await p.waitForTimeout(1500);

await p.getByRole("button", { name: "Author", exact: true }).click().catch((e) => console.log("author chip:", e.message));
await p.waitForTimeout(1500);
await p.screenshot({ path: "web-e2e/shots/watch/author-headers.png" });
console.log("author-headers shot");

const heads = await p.$$eval(".lib-cluster-head", (els) =>
  els.slice(0, 10).map((e) => e.innerText.replace(/\n/g, " ").trim()));
console.log("AUTHORS:", JSON.stringify(heads));

// Expand an author that has multiple series / gaps to show the sub-cluster layout
// with missing books merged in. Try a few known multi-series authors.
const candidates = ["Brandon Sanderson", "Matt Dinniman", "Jenn Lyons", "Andy Weir", "Dennis E. Taylor"];
let done = false;
for (const name of candidates) {
  const a = p.locator(".lib-cluster-head", { hasText: name }).first();
  if (await a.count()) {
    await a.click();
    await p.waitForTimeout(2000);
    await a.scrollIntoViewIfNeeded();
    await p.waitForTimeout(400);
    await p.screenshot({ path: "web-e2e/shots/watch/author-expanded.png" });
    const subs = await p.$$eval(".lib-subcluster-head", (els) => els.map((e) => e.innerText.trim()));
    console.log(`author-expanded shot: "${name}" subclusters=${JSON.stringify(subs.slice(0, 8))}`);
    done = true;
    break;
  }
}
if (!done) console.log("no candidate author found; headers shot only");
await b.close();
