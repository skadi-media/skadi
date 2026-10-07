// Accessibility baseline (SKADI-T-0700).
//
// 1. A keyboard-only pass: add → grab → watch → settings, with Tab, Shift+Tab,
//    Enter, Space and the `/` shortcut only. No mouse clicks. Fixtures that are
//    not part of the pass (domains, the library root, a quality profile) are
//    seeded through the API.
// 2. axe-core over the main pages of the built UI, served by the mock-provider
//    harness. A page fails on any "critical" or "serious" violation. Every
//    violation (moderate and minor too) is printed and written to
//    test-results/axe/<page>.json, so the log is the audit record.
//
// The keyboard pass runs first: the harness's metadata stub knows one movie
// (The Matrix), and the pass is the one that adds it.
//
// Run: `cd web-e2e && npx playwright test a11y` (the webServer builds the UI
// and starts the harness; see playwright.config.ts).
import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import * as fs from "fs";
import * as path from "path";
import * as steps from "./steps";

const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, "..", ".tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");



/** Wait until the page has finished its first loads (no skeleton left). */
async function settled(page: Page) {
  await expect(page.locator("main.main")).toBeVisible();
  await expect(page.locator(".skeleton")).toHaveCount(0, { timeout: 20_000 });
  await page.waitForTimeout(300);
}

/** Run axe on the current page; fail (softly) on critical/serious, print all. */
async function audit(page: Page, label: string) {
  const res = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "best-practice"])
    .analyze();
  const out = path.resolve(__dirname, "..", "test-results", "axe");
  fs.mkdirSync(out, { recursive: true });
  fs.writeFileSync(
    path.join(out, label.replace(/[^a-z0-9]+/gi, "_") + ".json"),
    JSON.stringify(res.violations, null, 1),
  );
  console.log(`axe ${label}: ${res.violations.length} rule(s) violated, ${res.passes.length} passed`);
  for (const v of res.violations) {
    const where = v.nodes
      .slice(0, 3)
      .map((n) => n.target.join(" "))
      .join(" | ");
    console.log(`  [${v.impact}] ${v.id} (${v.nodes.length}): ${where}`);
  }
  const blocking = res.violations.filter((v) => v.impact === "critical" || v.impact === "serious");
  expect.soft(blocking.map((v) => `${v.impact} ${v.id} (${v.nodes.length})`), label).toEqual([]);
}

/** A description of the focused element, for failure messages. */
async function focused(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el) return "(nothing)";
    const name = el.getAttribute("aria-label") || el.textContent?.trim().slice(0, 40) || "";
    return `<${el.tagName.toLowerCase()} class="${el.className}"> ${name}`;
  });
}

/** In the page: does the focused element show a ring? An outline or
 *  box-shadow on it or on its nearest two ancestors (a search bar draws the
 *  ring around its bare input). */
function ringInPage(): boolean {
  let el = document.activeElement as HTMLElement | null;
  for (let up = 0; el && up < 3; up++, el = el.parentElement) {
    const s = getComputedStyle(el);
    if ((s.outlineStyle !== "none" && parseFloat(s.outlineWidth) > 0) || s.boxShadow !== "none") return true;
  }
  return false;
}

async function expectRing(page: Page, what: string) {
  const ring = await page.evaluate(ringInPage);
  expect(ring, `${what} shows a focus ring (focus: ${await focused(page)})`).toBeTruthy();
}

/**
 * Press Tab (or Shift+Tab) until the focused element matches `selector` (and
 * its text matches `text`, when given). Every stop on the way must show a
 * focus ring.
 */
async function tabTo(page: Page, selector: string, opts: { back?: boolean; text?: RegExp } = {}) {
  for (let i = 0; i < 150; i++) {
    await page.keyboard.press(opts.back ? "Shift+Tab" : "Tab");
    const r = await page.evaluate(
      ([sel, src, ringSrc]) => {
        const el = document.activeElement;
        const ring = new Function(`return (${ringSrc})()`)() as boolean;
        if (!el || el === document.body) return { hit: false, body: true, ring };
        const hit = el.matches(sel) && (!src || new RegExp(src).test(el.textContent?.trim() ?? ""));
        return { hit, body: false, ring };
      },
      [selector, opts.text?.source ?? "", ringInPage.toString()] as const,
    );
    if (!r.body) expect(r.ring, `Tab stop ${i} towards ${selector} shows a focus ring (focus: ${await focused(page)})`).toBeTruthy();
    if (r.hit) return;
  }
  throw new Error(`Tab never reached ${selector} ${opts.text ?? ""}; focus is on ${await focused(page)}`);
}

async function seedBasics(request: APIRequestContext) {
  for (const d of ["movies", "audiobooks"]) await steps.givenDomainEnabled(request, d);
  const root = await request.put(`${steps.API}/config/library.root`, { data: { value: LIBRARY } });
  expect(root.ok()).toBeTruthy();
  const profiles = await (await request.get(`${steps.API}/settings/profiles`)).json();
  if (!Array.isArray(profiles) || profiles.length === 0) {
    const p = await request.post(`${steps.API}/settings/profiles`, { data: { name: "a11y", cutoff: "Bluray-1080p" } });
    expect(p.ok()).toBeTruthy();
  }
}

test.describe("C31 a11y: keyboard-only pass", () => {
  test("add → grab → watch → settings with the keyboard only", async ({ page, request }) => {
    test.setTimeout(240_000);
    await seedBasics(request);

    // --- add -------------------------------------------------------------
    await page.goto("/add");
    await settled(page);
    // The first Tab stop is the skip link; Enter puts the focus in <main>.
    await page.keyboard.press("Tab");
    await expect(page.locator(".skip-link")).toBeFocused();
    await expectRing(page, "skip link");
    await page.keyboard.press("Enter");
    await expect(page.locator("main")).toBeFocused();
    // `/` jumps to the search field.
    await page.keyboard.press("/");
    await expect(page.getByRole("textbox", { name: "Search" })).toBeFocused();
    await page.keyboard.type("Matrix");
    await page.keyboard.press("Enter");
    await expect(page.getByRole("button", { name: /^Add The Matrix/ })).toBeVisible({ timeout: 20_000 });
    await tabTo(page, 'button[aria-label^="Add The Matrix"]');
    await page.keyboard.press("Enter");
    await expect(page.locator(".toast-region")).toContainText("Added", { timeout: 20_000 });

    // --- grab ------------------------------------------------------------
    // Back into the sidebar with Shift+Tab, to the Movies link.
    await tabTo(page, '.nav a[href="/movies"]', { back: true });
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/movies$/);
    await settled(page);
    await tabTo(page, '.poster-tile[aria-label^="The Matrix"]');
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/movies\/[0-9a-f-]+$/);
    await settled(page);
    await tabTo(page, "button", { text: /^Search releases$/ });
    await page.keyboard.press("Enter");
    await expect(page.getByRole("button", { name: /^Grab The\.Matrix/ }).first()).toBeVisible({ timeout: 20_000 });
    await tabTo(page, 'button[aria-label^="Grab The.Matrix"]');
    await page.keyboard.press("Space");
    // Space pressed the Grab button: the live note answers. "+ Add" already
    // started an automatic search, so the answer can be that one is running.
    await expect(page.locator(".releases-note")).toContainText(/grab|in progress/i, { timeout: 20_000 });

    // --- watch -----------------------------------------------------------
    // The harness downloads and imports the grab; then the Watch link shows.
    const detail = page.url();
    await expect(async () => {
      await page.goto(detail);
      await expect(page.locator('a[href^="/watch/movie/"]').first()).toBeVisible({ timeout: 3_000 });
    }).toPass({ timeout: 90_000 });
    await settled(page);
    await tabTo(page, 'a[href^="/watch/movie/"]');
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/watch\/movie\//);
    // The player takes the focus (its native controls answer Space / arrows).
    await tabTo(page, "video");

    // --- settings ----------------------------------------------------------
    await tabTo(page, '.nav a[href="/indexers"]', { back: true });
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/indexers$/);
    await settled(page);
    await tabTo(page, "main button", { text: /^\+ Add$/ });
    await page.keyboard.press("Enter");
    const save = /^(Save|Add|Saving…|Adding…)$/;
    await tabTo(page, "main button", { text: save });
    // A blank required field: no request, the form says what is missing.
    await page.keyboard.press("Enter");
    await expect(page.locator(".field-error").first()).toBeVisible();
    await tabTo(page, "main button", { text: /^Cancel$/ });
    await page.keyboard.press("Enter");
    await expect(page.locator(".field-error")).toHaveCount(0);

    // `?` lists the shortcuts; Esc closes the list and the focus comes back.
    await page.keyboard.press("?");
    await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Close" })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toHaveCount(0);
  });
});

test.describe("C31 a11y: axe on the main pages", () => {
  test.beforeAll(async ({ request }) => {
    await seedBasics(request);
    await steps.givenTorznabIndexer(request, "axe-indexer");
    const movies = await (await request.get(`${steps.API}/movies`)).json();
    if (!Array.isArray(movies) || movies.length === 0) {
      const profiles = await (await request.get(`${steps.API}/settings/profiles`)).json();
      const r = await request.post(`${steps.API}/movies`, {
        data: { tmdb_id: 603, profile: profiles[0].id, root_folder: LIBRARY, search: false },
      });
      expect(r.ok()).toBeTruthy();
    }
  });

  const pages = [
    "/",
    "/add",
    "/movies",
    "/tv",
    "/audiobooks",
    "/activity",
    "/wanted",
    "/downloaders",
    "/indexers",
    "/config",
    "/household",
    "/naming",
    "/system",
    "/listen",
    "/players",
    "/upload",
    "/movies/import",
    "/audiobooks/discover",
  ];
  for (const p of pages) {
    test(`axe ${p}`, async ({ page }) => {
      await page.goto(p);
      await settled(page);
      await audit(page, p);
    });
  }

  test("axe the Add results", async ({ page }) => {
    await page.goto("/add");
    await settled(page);
    await page.getByRole("textbox", { name: "Search" }).fill("Matrix");
    await page.keyboard.press("Enter");
    await expect(page.locator(".add-row").first()).toBeVisible({ timeout: 20_000 });
    await audit(page, "/add (results)");
  });

  test("axe a movie detail page, its releases, and the watch page", async ({ page }) => {
    await page.goto("/movies");
    await settled(page);
    await page.locator(".poster-tile").first().click();
    await expect(page).toHaveURL(/\/movies\/[0-9a-f-]+$/);
    await settled(page);
    await audit(page, "/movies/:id");
    await page.getByRole("button", { name: "Search releases" }).click();
    await expect(page.locator(".releases-table").first()).toBeVisible({ timeout: 20_000 });
    await audit(page, "/movies/:id (releases)");
    const watch = page.locator('a[href^="/watch/movie/"]').first();
    if (await watch.count()) {
      await watch.click();
      await expect(page.locator("video")).toBeVisible();
      await audit(page, "/watch/movie/:id/:eid");
    }
  });

  test("axe the library in select mode and the shortcut list", async ({ page }) => {
    await page.goto("/movies");
    await settled(page);
    const select = page.getByRole("button", { name: /^Select$/ }).first();
    if (await select.count()) {
      await select.click();
      await audit(page, "/movies (select mode)");
      await page.reload();
      await settled(page);
    }
    await page.locator("main").focus();
    await page.keyboard.press("?");
    await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toBeVisible();
    await audit(page, "/movies (shortcut list)");
  });

  test("axe the indexer form", async ({ page }) => {
    await page.goto("/indexers");
    await settled(page);
    await page.getByRole("button", { name: "+ Add" }).first().click();
    await audit(page, "/indexers (add form)");
  });

  test("axe the narrow drawer; the page under it does not scroll", async ({ page }) => {
    await page.setViewportSize({ width: 375, height: 800 });
    await page.goto("/");
    await settled(page);
    await page.getByRole("button", { name: "Menu" }).click();
    await expect(page.locator("html")).toHaveClass(/nav-lock/);
    expect(await page.evaluate(() => getComputedStyle(document.body).overflow)).toBe("hidden");
    await audit(page, "/ (drawer open, 375px)");
    await page.keyboard.press("Escape");
    await expect(page.locator("html")).not.toHaveClass(/nav-lock/);
  });
});

test.describe("C31 accessibility.feature gaps", () => {
  // Moderate axe findings left open: page titles are <h2>, and the detail
  // page's edition headings jump from h2 to h4.
  test.fixme("@gap each page has one level-one heading", async () => {});
});
