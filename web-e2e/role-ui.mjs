// Behavioural UI tests per household role (filed after the 2026-09-24 report:
// a read-only member was shown "Search or add anything", "+ Add media" and a
// storage meter, all of which the API refuses).
//
// The load-bearing assertion is not "control X is hidden" — that is a
// restatement of the code. It is:
//
//     browsing normally as any role must produce ZERO 403s.
//
// The UI's job is never to offer what `path_allowed` refuses. That one check
// would have caught all three of the bugs above, and it keeps working for
// controls nobody has written yet.
import { chromium } from "@playwright/test";

const BASE = process.env.BASE || "http://127.0.0.1:8090";
const ADMIN = process.env.SKADI_TOKEN;
if (!ADMIN) { console.error("set SKADI_TOKEN to the api_token"); process.exit(2); }

const PW = "role-ui-harness-pw-1";
const ROLES = ["contributor", "member", "kid"];
// Pages a role can actually REACH, derived from the rendered nav rather than
// hardcoded. Typing an admin-only URL as a kid *should* 403 — that is the gate
// working, not a UI bug. What must never 403 is a page the UI itself offered.
const ALWAYS = ["/"];
const ADMIN_ONLY_DEEPLINKS = ["/indexers", "/config", "/household", "/naming",
                              "/downloaders", "/movies/config", "/audiobooks/config"];

async function api(path, opts = {}) {
  const r = await fetch(BASE + "/api/v1" + path, {
    ...opts,
    headers: { Authorization: `Bearer ${ADMIN}`, "content-type": "application/json", ...(opts.headers || {}) },
  });
  return { status: r.status, body: await r.text() };
}

// --- throwaway accounts, one per role ---------------------------------------
const made = [];
async function ensure(role) {
  const name = `uitest-${role}`;
  const r = await api("/members", { method: "POST", body: JSON.stringify({ name, role, password: PW }) });
  if (r.status !== 201) throw new Error(`creating ${name}: ${r.status} ${r.body.slice(0, 140)}`);
  const id = JSON.parse(r.body).member.id;
  made.push(id);
  return name;
}
async function cleanup() {
  for (const id of made) await api(`/members/${id}`, { method: "DELETE" });
}

async function tokenFor(username) {
  const r = await fetch(BASE + "/api/v1/auth/login", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ username, password: PW, label: "role-ui" }),
  });
  if (!r.ok) throw new Error(`login ${username}: ${r.status}`);
  return (await r.json()).token;
}

// --- the sweep ---------------------------------------------------------------
async function sweep(browser, label, token) {
  const page = await (await browser.newContext({ viewport: { width: 1440, height: 1000 } })).newPage();
  const denied = [];
  page.on("response", r => {
    if (r.status() === 403 && r.url().includes("/api/")) {
      denied.push(`${r.request().method()} ${r.url().replace(BASE, "")}`);
    }
  });
  await page.goto(BASE + "/", { waitUntil: "domcontentloaded" });
  await page.evaluate(t => localStorage.setItem("skadi.token", t), token);

  // Ask the page which destinations this role is offered.
  await page.goto(BASE + "/", { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(3000);
  const offered = await page.evaluate(() =>
    [...document.querySelectorAll("nav.nav a[href], .subnav-link")]
      .map(a => a.getAttribute("href"))
      .filter(h => h && h.startsWith("/")));
  const pages = [...new Set([...ALWAYS, ...offered])]
    .filter(p => !ADMIN_ONLY_DEEPLINKS.includes(p));

  const seen = {};
  // Phase 2 targets: every in-page link the role is *shown*, not just the nav.
  // The nav-only walk missed a whole class — Import, the settings gear and
  // Discover are buttons in a library page's header, and an author name is a
  // link to a detail page. All of them fired 403s for non-admins while this
  // harness reported clean (SKADI-T-0639). A rendered link is an offer, so it
  // is in scope exactly like a nav entry: no ADMIN_ONLY_DEEPLINKS filter here,
  // because a member being *shown* /movies/import is the bug itself.
  const discovered = new Set();
  const visit = async (path) => {
    await page.goto(BASE + path, { waitUntil: "domcontentloaded" });
    await page.waitForTimeout(2500);
    for (const h of await page.evaluate(() =>
      [...document.querySelectorAll("a[href^='/']")].map(a => a.getAttribute("href")))) {
      // Never follow sign-out: it would invalidate the token mid-sweep and
      // every later page would 401, masking whatever we came to find.
      if (h && !/logout|signout/i.test(h)) discovered.add(h);
    }
    return {
      addMedia: await page.locator("button.add-media").count(),
      ovSearch: await page.locator("button.ov-search").count(),
      storage: await page.locator(".storage-row").count(),
      subnav: (await page.locator(".subnav-link").allTextContents()).join("|"),
    };
  };

  for (const path of pages) seen[path] = await visit(path);

  // One hop past the nav. Capped so a library of a few hundred items does not
  // turn this into a crawl — a handful of detail pages exercises the same code.
  const extra = [...discovered].filter(p => !pages.includes(p)).slice(0, 30);
  for (const path of extra) seen[path] = await visit(path);

  await page.close();
  return { denied: [...new Set(denied)], seen, pages, extra };
}

const browser = await chromium.launch();
let failures = 0;
try {
  const subjects = [["admin", ADMIN]];
  for (const role of ROLES) subjects.push([role, await tokenFor(await ensure(role))]);

  for (const [label, token] of subjects) {
    const { denied, seen, pages, extra } = await sweep(browser, label, token);
    console.log(`\n=== ${label} ===`);
    console.log(`  reachable pages:      ${pages.join(" ")}`);
    console.log(`  followed in-page:     ${extra.join(" ") || "(none)"}`);
    console.log(`  add-media button on:  ${Object.entries(seen).filter(([, v]) => v.addMedia).map(([k]) => k).join(" ") || "(none)"}`);
    console.log(`  overview search:      ${seen["/"]?.ovSearch ? "shown" : "hidden"}`);
    console.log(`  storage meter:        ${seen["/"]?.storage ? "shown" : "hidden"}`);
    console.log(`  activity strip:       ${seen["/activity"]?.subnav || "(none)"}`);
    if (denied.length) {
      failures++;
      console.log(`  403s (${denied.length}) — the UI offered what the API refuses:`);
      for (const d of denied) console.log(`    ${d}`);
    } else {
      console.log("  403s: none");
    }
  }
} finally {
  await cleanup();
  await browser.close();
}
console.log(failures ? `\nFAIL: ${failures} role(s) hit a 403` : "\nPASS: no role was offered anything the API refuses");
process.exit(failures ? 1 : 0);
