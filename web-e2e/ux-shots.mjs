// Screenshot every page of the LIVE deploy for a UX review (SKADI-T-0580).
// Real library, real data — output is gitignored; do not commit the images.
//   node web-e2e/ux-shots.mjs [outdir]
import { chromium } from "@playwright/test";
import { mkdirSync } from "node:fs";

const BASE = process.env.SKADI_URL || "http://127.0.0.1:8090";
// Against a `trunk serve` dev build the page still carries the token placeholder
// (only the daemon substitutes it), so swap it in ourselves when a token is given.
const TOKEN = process.env.SKADI_TOKEN || "";
const OUT = process.argv[2] || "web-e2e/shots/ux";
mkdirSync(OUT, { recursive: true });

const MOVIE = "79289020-c02a-40a8-80ce-1cd09188077f";
const SERIES = "3e943105-51b5-4109-a2cd-69b26457d59e";
const BOOK = "d127e315-deee-4f26-a1cf-07560c02a854";

const ROUTES = [
  ["overview", "/"],
  ["activity", "/activity"],
  ["add", "/add"],
  ["audiobooks", "/audiobooks"],
  ["book", `/audiobooks/${BOOK}`],
  ["audiobooks-discover", "/audiobooks/discover"],
  ["audiobooks-import", "/audiobooks/import"],
  ["audiobooks-config", "/audiobooks/config"],
  ["movies", "/movies"],
  ["movie", `/movies/${MOVIE}`],
  ["movies-import", "/movies/import"],
  ["movies-config", "/movies/config"],
  ["tv", "/tv"],
  ["series", `/tv/${SERIES}`],
  ["tv-import", "/tv/import"],
  ["listen", "/listen"],
  ["indexers", "/indexers"],
  ["downloaders", "/downloaders"],
  ["naming", "/naming"],
  ["config", "/config"],
];

const VIEWPORTS = {
  desktop: { width: 1440, height: 900 },
  phone: { width: 414, height: 896 },
};

const browser = await chromium.launch({ channel: "chrome" });
for (const [vpName, viewport] of Object.entries(VIEWPORTS)) {
  const ctx = await browser.newContext({ viewport, deviceScaleFactor: 2, isMobile: vpName === "phone" });
  const page = await ctx.newPage();
  if (TOKEN) {
    // The document is not always fetched through a route (cache), so patch the
    // meta tag in-page instead: readyState "interactive" fires after parsing and
    // before the deferred module script that reads it.
    await page.addInitScript((token) => {
      const patch = () => {
        const m = document.querySelector('meta[name="skadi-api-token"]');
        if (m && m.content === "__SKADI_API_TOKEN__") m.content = token;
      };
      document.addEventListener("readystatechange", patch);
      new MutationObserver(patch).observe(document, { childList: true, subtree: true });
    }, TOKEN);
  }
  const errs = [];
  page.on("console", (m) => m.type() === "error" && errs.push(m.text()));
  for (const [name, route] of ROUTES) {
    await page.goto(BASE + route, { waitUntil: "networkidle", timeout: 30000 }).catch(() => {});
    await page.waitForTimeout(1800);
    const h = await page.evaluate(() => document.documentElement.scrollHeight);
    await page.screenshot({ path: `${OUT}/${vpName}-${name}.png`, fullPage: h < 6000 });
    console.log(`${vpName} ${name.padEnd(20)} ${route.padEnd(52)} h=${h}`);
  }
  if (errs.length) console.log(`${vpName} console errors:`, [...new Set(errs)].slice(0, 8));
  await ctx.close();
}
await browser.close();
