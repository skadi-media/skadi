// The per-role sweep (SKADI-T-0639): the UI must never offer what the API
// refuses. A release is not cut while this fails (`angreal version bump` runs
// it first).
//
// For each role that is not the admin (the admin short-circuits
// `household::path_allowed`, so it never reproduces a role bug), a throwaway
// member signs in and the sweep:
//
// - opens every page the role is offered: the sidebar, the section strips and
//   every in-page link, starting from the Overview;
// - fails when a linked page is one the role may not use (the page's call, in
//   PAGES below, is refused for the role);
// - presses every control the page offers (buttons, toggles, form submits,
//   and the confirm button of a dialog a control opens);
// - fails on any API answer of 401 or 403, on any request the client gate
//   stopped (`skadi: not sent for this role:`, SKADI-T-0642), and on any write
//   a control sent that the server's gate refuses for the role.
//
// Writes never reach the daemon: every request that is not a GET or HEAD is
// answered in the browser, and the harness's `POST /e2e/gate` says what the
// server's gate would decide for it. So pressing Delete deletes nothing.
import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import * as path from "path";
import * as steps from "./steps";

const API = steps.API;
const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, "..", ".tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");
const ASIN = "B08G9PRS1K";
const GATE_PREFIX = "skadi: not sent for this role:";

type Role = "member" | "contributor" | "kid";
const ROLES: Role[] = ["member", "contributor", "kid"];

/**
 * Every route of `crates/skadi-web/src/app.rs`, and the API call that says
 * whether a role may use its page: the page's own read, or for a page that
 * exists to change something, the write it is for. `subnav::page_call` names
 * the same calls for the section strips. A linked route missing here fails
 * the sweep, so a new page cannot go unchecked.
 */
const PAGES: [RegExp, string, string][] = [
  [/^\/$/, "GET", "/me"],
  [/^\/preferences$/, "GET", "/me"],
  [/^\/add$/, "GET", "/movies/lookup"],
  [/^\/setup$/, "POST", "/settings/indexers"],
  [/^\/activity$/, "GET", "/activity"],
  [/^\/wanted$/, "GET", "/wanted"],
  [/^\/players$/, "GET", "/players"],
  [/^\/upload$/, "POST", "/uploads"],
  [/^\/movies$/, "GET", "/movies"],
  [/^\/movies\/import$/, "POST", "/library-import/scan"],
  [/^\/movies\/config$/, "POST", "/settings/profiles"],
  [/^\/movies\/[^/]+$/, "GET", "/movies/x"],
  [/^\/tv$/, "GET", "/series"],
  [/^\/tv\/import$/, "POST", "/tv/library-import/scan"],
  [/^\/tv\/[^/]+$/, "GET", "/series/x"],
  [/^\/audiobooks$/, "GET", "/books"],
  [/^\/audiobooks\/import$/, "POST", "/audiobooks/library-import/scan"],
  [/^\/audiobooks\/config$/, "POST", "/settings/profiles"],
  [/^\/audiobooks\/discover$/, "GET", "/audiobooks/discover"],
  [/^\/audiobooks\/authors\/[^/]+$/, "GET", "/authors/x"],
  [/^\/audiobooks\/[^/]+$/, "GET", "/books/x"],
  [/^\/listen$/, "GET", "/listen"],
  [/^\/listen\/[^/]+\/[^/]+$/, "GET", "/books/x/files/y/audio"],
  [/^\/watch\/movie\/[^/]+\/[^/]+$/, "GET", "/movies/x/editions/y/video"],
  [/^\/watch\/tv\/[^/]+\/[^/]+$/, "GET", "/series/x/episodes/y/video"],
  [/^\/indexers$/, "GET", "/settings/indexers"],
  [/^\/downloaders$/, "GET", "/settings/downloaders"],
  [/^\/config$/, "GET", "/config"],
  [/^\/household$/, "GET", "/members"],
  [/^\/naming$/, "GET", "/naming/settings"],
  [/^\/importlists$/, "GET", "/importlists"],
  [/^\/system$/, "GET", "/system/status"],
];

/** Controls the sweep does not press: signing out would end the sweep's own
 * session, and the skip link only moves the focus. */
const NOT_PRESSED = /^(Sign out|Skip to content)$/;

/** The route pattern of a page, so the sweep opens one film, not every film. */
function pattern(p: string): RegExp | undefined {
  return PAGES.find(([re]) => re.test(p))?.[0];
}

/** The nested API path of a request URL: `/api/v1/movies/1?x=y` -> `/movies/1`. */
function apiPath(url: string): string {
  const u = new URL(url, "http://x");
  return u.pathname.replace(/^\/api\/v1/, "");
}

/** What the server's gate decides for `role` calling `method path`, asked of
 * the harness (`POST /e2e/gate` runs `household::path_allowed`). */
function gateFor(request: APIRequestContext, admin: string, role: Role) {
  const cache = new Map<string, boolean>();
  return async (method: string, p: string): Promise<boolean> => {
    const key = `${method} ${p}`;
    const hit = cache.get(key);
    if (hit !== undefined) return hit;
    const r = await request.post(`${API}/e2e/gate`, {
      headers: { Authorization: `Bearer ${admin}` },
      data: { role, method, path: p },
    });
    expect(r.ok(), `gate ${key}: ${r.status()}`).toBeTruthy();
    const allowed = (await r.json()).allowed as boolean;
    cache.set(key, allowed);
    return allowed;
  };
}

/** Seed one film and one book (with its author), so the detail pages and
 * their controls are on offer. Open mode: the caller is the admin. Reuses a
 * copy an earlier spec left. */
async function seed(request: APIRequestContext) {
  await steps.givenDomainEnabled(request, "movies");
  await steps.givenDomainEnabled(request, "audiobooks");
  const root = await request.put(`${API}/config/library.root`, { data: { value: LIBRARY } });
  expect(root.ok(), `set library.root: ${root.status()}`).toBeTruthy();
  const movies = (await (await request.get(`${API}/movies`)).json()) as unknown[];
  const books = (await (await request.get(`${API}/books`)).json()) as unknown[];
  if (movies.length && books.length) return;
  const profile = await request.post(`${API}/settings/profiles`, {
    data: { name: "e2e-roles", cutoff: "Bluray-1080p" },
  });
  expect(profile.ok(), `create profile: ${profile.status()}`).toBeTruthy();
  const profileId = (await profile.json()).id as string;
  if (!movies.length) {
    const r = await request.post(`${API}/movies`, {
      data: { tmdb_id: 603, profile: profileId, root_folder: LIBRARY, search: false },
    });
    expect(r.ok(), `add movie: ${r.status()}`).toBeTruthy();
  }
  if (!books.length) {
    const r = await request.post(`${API}/books`, {
      data: { asin: ASIN, profile: profileId, root: LIBRARY, search: false },
    });
    expect(r.ok(), `add book: ${r.status()}`).toBeTruthy();
  }
}

/** Mark each visible, enabled control with a key (its name, and its place
 * among controls of the same name) and return the keys, in page order. */
async function markControls(page: Page, scope: string): Promise<string[]> {
  return page.evaluate(
    ({ scope, skip }) => {
      const root = document.querySelector(scope);
      if (!root) return [];
      const sel =
        "button, [role=button], [role=link]:not([href]), [role=switch], [role=checkbox], [role=radio], [role=tab], [role=menuitem], summary, input[type=submit], input[type=button], input[type=checkbox], input[type=radio], select";
      const counts = new Map<string, number>();
      const keys: string[] = [];
      for (const el of Array.from(root.querySelectorAll<HTMLElement>(sel))) {
        if (el.closest("a[href]")) continue; // a link styled as a button is a link
        const r = el.getBoundingClientRect();
        const shown = r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== "hidden";
        if (!shown || (el as HTMLButtonElement).disabled || el.getAttribute("aria-disabled") === "true") continue;
        const name = (
          el.getAttribute("aria-label") ||
          el.textContent ||
          el.getAttribute("title") ||
          (el as HTMLInputElement).value ||
          el.getAttribute("name") ||
          el.tagName
        )
          .replace(/\s+/g, " ")
          .trim()
          .slice(0, 60);
        if (new RegExp(skip).test(name)) continue;
        const n = counts.get(name) ?? 0;
        counts.set(name, n + 1);
        const key = `${name}#${n}`;
        el.setAttribute("data-sweep-key", key);
        keys.push(key);
      }
      return keys;
    },
    { scope, skip: NOT_PRESSED.source },
  );
}

/** Every same-origin link on the page: routes, and links straight to the API. */
async function links(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLAnchorElement>("a[href]"))
      .map((a) => new URL(a.href, location.href))
      .filter((u) => u.origin === location.origin && !u.hash.startsWith("#main"))
      .map((u) => u.pathname),
  );
}

const DIALOG = "[role=dialog], [role=alertdialog], dialog[open]";

async function dialogOpen(page: Page): Promise<boolean> {
  return (await page.locator(DIALOG).filter({ visible: true }).count()) > 0;
}

/** Sweep the pages offered to `role`; returns the findings. */
async function sweep(page: Page, role: Role, gate: (m: string, p: string) => Promise<boolean>) {
  const findings = new Set<string>();
  let where = "/";
  let inFlight = 0;

  page.on("dialog", (d) => void d.dismiss());
  page.on("console", (m) => {
    const t = m.text();
    const at = t.indexOf(GATE_PREFIX);
    if (at >= 0) findings.add(`${where}: stopped in the browser: ${t.slice(at + GATE_PREFIX.length).trim()}`);
  });
  page.on("response", (r) => {
    if (r.url().includes(`${API}/`) && (r.status() === 401 || r.status() === 403)) {
      findings.add(`${where}: ${r.status()} ${r.request().method()} ${apiPath(r.url())}`);
    }
  });
  page.on("request", (r) => {
    if (r.url().includes(`${API}/`) && !r.url().includes(`${API}/events`)) inFlight++;
  });
  const done = (r: { url(): string }) => {
    if (r.url().includes(`${API}/`) && !r.url().includes(`${API}/events`)) inFlight = Math.max(0, inFlight - 1);
  };
  page.on("requestfinished", done);
  page.on("requestfailed", done);

  // No write reaches the daemon: answer it here, and ask the gate about it.
  const writes: { at: string; method: string; path: string }[] = [];
  await page.route(`**${API}/**`, async (route) => {
    const req = route.request();
    const method = req.method();
    if (method === "GET" || method === "HEAD") return route.continue();
    writes.push({ at: where, method, path: apiPath(req.url()) });
    return route.fulfill({ status: 200, contentType: "application/json", body: "{}" });
  });

  const settle = async () => {
    await page.waitForTimeout(150);
    for (let i = 0; i < 40 && inFlight > 0; i++) await page.waitForTimeout(50);
  };

  const open = async (p: string) => {
    where = p;
    await page.goto(p);
    await expect(page.locator(".nav")).toBeVisible();
    // The shell asks `/me` first; the page's own reads follow it.
    await settle();
    await page.waitForTimeout(300);
    await settle();
  };

  const queue: string[] = ["/"];
  const opened = new Set<RegExp | string>();
  const checked = new Set<string>();
  const visited: string[] = [];

  // A link (or a navigation a control made) is an offer: the role must be
  // able to use the page, and the sweep opens it.
  const offer = async (from: string, p: string) => {
    if (p.startsWith(`${API}/`)) {
      if (!(await gate("GET", apiPath(p)))) findings.add(`${from}: links to ${p}, refused for ${role}`);
      return;
    }
    if (checked.has(p)) return;
    checked.add(p);
    const entry = PAGES.find(([re]) => re.test(p));
    if (!entry) {
      findings.add(`${from}: links to ${p}, a route missing from PAGES in roles.spec.ts`);
      return;
    }
    if (!(await gate(entry[1], entry[2]))) {
      findings.add(`${from}: links to ${p}, a page ${role} may not use (${entry[1]} ${entry[2]})`);
      return;
    }
    queue.push(p);
  };

  let presses = 0;
  while (queue.length) {
    const p = queue.shift()!;
    const pat = pattern(p) ?? p;
    if (opened.has(pat)) continue;
    opened.add(pat);
    visited.push(p);
    await open(p);
    for (const l of await links(page)) await offer(p, l);

    // Press each control once. A press can change the page (a toggle shows
    // more controls, a dialog opens, the route changes), so the controls are
    // marked again after each press and new ones join the list.
    const todo = await markControls(page, "body");
    const done = new Set<string>();
    for (let i = 0; i < todo.length && i < 80; i++) {
      const key = todo[i];
      if (done.has(key)) continue;
      done.add(key);
      let el = page.locator(`[data-sweep-key="${key.replace(/"/g, '\\"')}"]`);
      if (!(await el.count())) {
        await open(p);
        await markControls(page, "body");
        el = page.locator(`[data-sweep-key="${key.replace(/"/g, '\\"')}"]`);
        if (!(await el.count())) continue;
      }
      where = `${p} [${key}]`;
      presses++;
      try {
        await el.first().click({ timeout: 1500 });
      } catch {
        continue; // covered or gone: not an offer the role can take
      }
      await settle();
      // A dialog the press opened: press its confirm (the button that is not
      // Cancel or Close), which is where a destructive control's request is.
      if (await dialogOpen(page)) {
        const dlg = page.locator(DIALOG).filter({ visible: true }).last();
        for (const b of await dlg.getByRole("button").all()) {
          const name = ((await b.getAttribute("aria-label")) || (await b.textContent()) || "").trim();
          if (/^(Cancel|Close|×|✕|Back)$/i.test(name) || !(await b.isEnabled())) continue;
          where = `${p} [${key} > ${name}]`;
          try {
            await b.click({ timeout: 1500 });
          } catch {
            /* the dialog moved on */
          }
          await settle();
          break;
        }
        if (await dialogOpen(page)) await page.keyboard.press("Escape");
      }
      const now = new URL(page.url()).pathname;
      if (now !== p) {
        await offer(`${p} [${key}]`, now);
        await open(p);
      }
      for (const k of await markControls(page, "body")) if (!todo.includes(k)) todo.push(k);
    }
    where = p;
  }

  for (const w of writes) {
    if (!(await gate(w.method, w.path))) findings.add(`${w.at}: sent ${w.method} ${w.path}, refused for ${role}`);
  }
  return {
    findings: [...findings].sort(),
    opened: [...visited],
    presses,
    writes: [...new Set(writes.map((w) => `${w.method} ${w.path}`))],
  };
}

test.describe("SKADI-T-0639 per-role sweep", () => {
  for (const role of ROLES) {
    test(`@passing a ${role} is offered nothing the API refuses`, async ({ page, request }) => {
      test.setTimeout(180_000);
      await seed(request);
      const { restore, admin } = await steps.signInAs(request, page, role);
      let result;
      try {
        result = await sweep(page, role, gateFor(request, admin, role));
      } finally {
        await restore();
      }
      console.log(
        `${role}: ${result.opened.length} pages opened (${result.opened.join(" ")}), ` +
          `${result.presses} controls pressed, writes answered in the browser: ${result.writes.join(", ") || "none"}`,
      );
      // The sweep must have reached the seeded detail pages, or a pass means
      // nothing: a crawl that stopped at the Overview finds no findings.
      const reached = (re: RegExp) => result.opened.some((p) => re.test(p) && !/\/(import|config|discover)$/.test(p));
      expect(reached(/^\/movies\/[^/]+$/), `${role} reached a film's page`).toBeTruthy();
      if (role !== "kid") expect(reached(/^\/audiobooks\/[^/]+$/), `${role} reached a book's page`).toBeTruthy();
      expect(result.findings, `${role} was offered what the API refuses`).toEqual([]);
    });
  }
});
