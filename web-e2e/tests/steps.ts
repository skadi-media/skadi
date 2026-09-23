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
