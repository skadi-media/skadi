import { test, expect } from "@playwright/test";
import * as path from "path";

// Mirrors steer.spec.ts for the audiobooks domain. The harness serves BOTH the
// movies and audiobooks domains; this spec drives the audiobooks UI end-to-end:
// add a book by ASIN (via API) → open its detail → interactive search lists the
// mock release → grab → the book file reaches "imported" → dashboard renders.
//
// The harness's FakeAudnexus answers any `GET /books/{asin}` with a canned
// "Project Hail Mary" record, so add-by-ASIN succeeds offline. The mock indexer
// returns one M4B-128 release; search:false keeps the only acquire the manual
// grab below (deterministic).
const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, "..", ".tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");
const API = "/api/v1";
const ASIN = "B08G9PRS1K";

test("audiobooks: add → search → grab → import → dashboard", async ({ page, request }) => {
  // --- setup via API (plumbing, not the thing under test) ---
  const profileResp = await request.post(`${API}/settings/profiles`, {
    data: { name: "e2e-ab", cutoff: "Bluray-1080p" },
  });
  expect(profileResp.ok()).toBeTruthy();
  const profileId = (await profileResp.json()).id as string;

  // The single library root is a config key, not a settings list (SKADI-T-0302
  // removed `root_folders`; SKADI-T-0461 fixed this seed). Seeding the old
  // endpoint 404'd and failed the suite at its first expect.
  expect(
    (await request.put(`${API}/config/library.root`, { data: { value: LIBRARY } })).ok(),
  ).toBeTruthy();
  expect(
    (await request.put(`${API}/domains/audiobooks`, { data: { enabled: true } })).ok(),
  ).toBeTruthy();
  // search:false so the only acquire is the manual grab below (deterministic).
  const addResp = await request.post(`${API}/books`, {
    data: { asin: ASIN, profile: profileId, root: LIBRARY, search: false },
  });
  expect(addResp.ok()).toBeTruthy();

  // --- open the book detail page via the Audiobooks sidebar link ---
  await page.goto("/");
  await page.locator(".nav").getByRole("link", { name: "Audiobooks" }).click();
  await expect(page.locator(".poster-tile").first()).toBeVisible();
  await page.locator(".poster-tile").first().click();
  await expect(page.locator(".drawer-title")).toContainText("Project Hail Mary");

  // --- interactive search lists the mock release; grab it ---
  await page.getByRole("button", { name: /Search releases/ }).click();
  const rel = page.locator(".drawer-release").first();
  await expect(rel).toContainText("Project Hail Mary");
  await rel.getByRole("button", { name: "Grab" }).click();
  await expect(page.locator(".drawer-note")).toContainText("Grabbed");

  // --- the immediate downloader imports it; the library reads "owned" on reload ---
  await expect(async () => {
    await page.goto("/audiobooks");
    await expect(page.getByRole("button", { name: /Owned/ })).toContainText("1");
  }).toPass({ timeout: 40_000 });

  // --- the Overview renders ---
  await page.locator(".nav").getByRole("link", { name: "Overview" }).click();
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
});
