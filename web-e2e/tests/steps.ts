// Given/When/Then helpers backing crates/skadi-web/tests/features/C31-web-ui/*.feature
// (SKADI-I-0057 pass P6). Each helper is one Gherkin step; the spec file
// (c31-web-ui.spec.ts) composes them scenario by scenario so the feature text
// stays the executable spec. Runs against the mock-provider harness started by
// playwright.config.ts (`angreal test e2e-web`).
import { expect, type APIRequestContext, type Page, type Request } from "@playwright/test";

export const API = "/api/v1";

// ---------------------------------------------------------------- Given ----

/** Given the harness daemon with the <domain> domain enabled */
export async function givenDomainEnabled(request: APIRequestContext, domain: string) {
  const r = await request.put(`${API}/domains/${domain}`, { data: { enabled: true } });
  expect(r.ok(), `enable ${domain}: ${r.status()}`).toBeTruthy();
}

/** Given a stored torznab indexer <name> (pointing at a closed port) with an api key */
export async function givenTorznabIndexer(request: APIRequestContext, name: string) {
  const r = await request.post(`${API}/settings/indexers`, {
    data: {
      kind: "torznab",
      name,
      base_url: "http://127.0.0.1:1",
      categories: [2000],
      api_key: "k",
    },
  });
  expect(r.ok(), `create indexer ${name}: ${r.status()}`).toBeTruthy();
  return (await r.json()).id as string;
}

/** Given the movie <title> (tmdb <id>) is added without an automatic search */
export async function givenMovieAdded(
  request: APIRequestContext,
  tmdbId: number,
  rootFolder: string,
) {
  const profile = await request.post(`${API}/settings/profiles`, {
    data: { name: "e2e", cutoff: "Bluray-1080p" },
  });
  expect(profile.ok()).toBeTruthy();
  const profileId = (await profile.json()).id as string;
  const r = await request.post(`${API}/movies`, {
    data: { tmdb_id: tmdbId, profile: profileId, root_folder: rootFolder, search: false },
  });
  expect(r.ok(), `add movie: ${r.status()}`).toBeTruthy();
}

/**
 * Given the harness asks for sign-in, and a household member with <role> is
 * signed in on this browser (SKADI-T-0723).
 *
 * The harness runs in open mode, where every caller is the admin. This sets
 * `api_token` (the harness applies it within a moment, as the daemon does),
 * makes the member with the admin token, and puts the member's own token where
 * the UI keeps it. The returned function puts the harness back in open mode:
 * call it in a `finally`, or every later test meets a 401.
 */
export async function givenSignedInAs(
  request: APIRequestContext,
  page: Page,
  role: "member" | "contributor" | "kid",
): Promise<() => Promise<void>> {
  const admin = `e2e-admin-${Date.now()}`;
  const auth = { Authorization: `Bearer ${admin}` };
  const set = await request.put(`${API}/config/api_token`, { data: { value: admin } });
  expect(set.ok(), `set api_token: ${set.status()}`).toBeTruthy();
  const restore = async () => {
    await request.delete(`${API}/config/api_token`, { headers: auth });
    await expect
      .poll(async () => (await request.get(`${API}/me`)).status(), { timeout: 10_000 })
      .toBe(200);
  };
  try {
    await expect
      .poll(async () => (await request.get(`${API}/me`)).status(), { timeout: 10_000 })
      .toBe(401);
    const made = await request.post(`${API}/members`, {
      headers: auth,
      data: { name: `Robin ${role} ${Date.now()}`, role },
    });
    expect(made.ok(), `create ${role}: ${made.status()}`).toBeTruthy();
    const token = (await made.json()).token as string;
    const me = await request.get(`${API}/me`, { headers: { Authorization: `Bearer ${token}` } });
    expect((await me.json()).role).toBe(role);
    await page.goto("/");
    await page.evaluate((t) => localStorage.setItem("skadi.token", t), token);
  } catch (e) {
    await restore();
    throw e;
  }
  return restore;
}

// ----------------------------------------------------------------- When ----

/** When the operator opens <path> — also records every API request the page makes. */
export async function whenOpens(page: Page, path: string): Promise<Request[]> {
  const seen: Request[] = [];
  page.on("request", (req) => {
    if (req.url().includes(`${API}/`)) seen.push(req);
  });
  await page.goto(path);
  await expect(page.locator(".nav")).toBeVisible();
  return seen;
}

/** When the operator clicks <label> (a button) */
export async function whenClicks(page: Page, label: string | RegExp) {
  await page.getByRole("button", { name: label }).first().click();
}

/** When the person opens their own settings with the gear in the top bar (SKADI-T-0723) */
export async function whenOpensSettingsByGear(page: Page) {
  await gear(page).click();
  await expect(page).toHaveURL(/\/preferences$/);
  await expect(page.getByRole("heading", { name: "Appearance" })).toBeVisible();
}

/** When the person picks the theme <label> on their own settings page (SKADI-T-0720) */
export async function whenPicksTheme(page: Page, label: string) {
  await themeToggle(page).getByRole("button", { name: label, exact: true }).click();
}

// ----------------------------------------------------------------- Then ----

/** Then the sidebar shows a link <name> */
export async function thenSidebarLink(page: Page, name: string, visible = true) {
  const link = page.locator(".nav").getByRole("link", { name, exact: true });
  if (visible) await expect(link).toBeVisible();
  else await expect(link).toHaveCount(0);
}

/** Then every request the page made to /api/v1/ carried an Authorization header */
export function thenAllRequestsAuthed(seen: Request[]) {
  expect(seen.length, "the page made at least one API call").toBeGreaterThan(0);
  for (const req of seen) {
    const auth = req.headers()["authorization"];
    expect(auth, `${req.method()} ${req.url()} had no Authorization header`).toMatch(/^Bearer /);
  }
}

/** Then a health badge named <name> shows status <status> */
export async function thenHealthBadge(page: Page, name: string, status: "ok" | "warn" | "fail") {
  const chip = page.locator(".health-chip", { has: page.locator(".health-chip-name", { hasText: name }) });
  await expect(chip.first()).toBeVisible();
  await expect(chip.first().locator(".health-dot")).toHaveClass(new RegExp(`\\b${status}\\b`));
}

/** Then the page does not show an error dialog */
export async function thenNoErrorDialog(page: Page) {
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
}

/** The gear in the top bar, to your own settings (SKADI-T-0723). */
export function gear(page: Page) {
  return page.locator(".cl-appshell__header").getByRole("link", { name: "Settings", exact: true });
}

/** Then the top bar shows the gear, and Tab reaches it */
export async function thenGearShown(page: Page) {
  await expect(gear(page)).toBeVisible();
  await expect(gear(page)).toHaveAttribute("href", "/preferences");
  // From the top of the page, Tab gets there: the skip link, the brand and
  // the bar's own controls come first.
  await page.locator("body").focus();
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  let reached = false;
  for (let i = 0; i < 12 && !reached; i++) {
    await page.keyboard.press("Tab");
    reached = await gear(page).evaluate((el) => el === document.activeElement);
  }
  expect(reached, "Tab reaches the gear").toBeTruthy();
}

/** Aurora's theme toggle on your own settings page (SKADI-T-0720). */
export function themeToggle(page: Page) {
  return page.getByRole("group", { name: "Theme" });
}

/** Then the theme toggle offers <labels>, in order, and marks <marked> only */
export async function thenThemeToggle(page: Page, labels: string[], marked: string) {
  const buttons = themeToggle(page).getByRole("button");
  await expect(buttons).toHaveText(labels);
  for (const label of labels) {
    await expect(themeToggle(page).getByRole("button", { name: label, exact: true })).toHaveAttribute(
      "aria-pressed",
      label === marked ? "true" : "false",
    );
  }
}

/** Then <html> carries data-theme <theme>, or none for null (System) */
export async function thenDataTheme(page: Page, theme: string | null) {
  const html = page.locator("html");
  if (theme === null) await expect(html).not.toHaveAttribute("data-theme", /.*/);
  else await expect(html).toHaveAttribute("data-theme", theme);
}

/** The page background now, as the browser computes it */
export async function pageBackground(page: Page): Promise<string> {
  return page.evaluate(() => getComputedStyle(document.body).backgroundColor);
}
