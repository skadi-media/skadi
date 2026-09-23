import { test, expect } from "@playwright/test";
import * as path from "path";

// The harness writes its library + fixture under WORKDIR (shared via env with
// playwright.config.ts). The spec points a root folder at <WORKDIR>/library so
// the grabbed fixture imports there.
const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, "..", ".tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");
const API = "/api/v1";

test("steer-it: add → search → block/unblock → grab → import → dashboard", async ({
  page,
  request,
}) => {
  // --- setup via API (plumbing, not the thing under test) ---
  const profileResp = await request.post(`${API}/settings/profiles`, {
    data: { name: "e2e", cutoff: "Bluray-1080p" },
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
    (await request.put(`${API}/domains/movies`, { data: { enabled: true } })).ok(),
  ).toBeTruthy();
  // search:false so the only acquire is the manual grab below (deterministic).
  expect(
    (
      await request.post(`${API}/movies`, {
        data: { tmdb_id: 603, profile: profileId, root_folder: LIBRARY, search: false },
      })
    ).ok(),
  ).toBeTruthy();

  // --- open the item drawer (the redesigned detail) from a library tile ---
  await page.goto("/");
  await page.locator(".nav").getByRole("link", { name: "Movies" }).click();
  await expect(page.locator(".poster-tile").first()).toBeVisible();
  await page.locator(".poster-tile").first().click();
  await expect(page.locator(".drawer-title")).toContainText("The Matrix");

  // --- interactive search lists the mock release; grab it ---
  await page.getByRole("button", { name: /Search releases/ }).click();
  const rel = page.locator(".drawer-release").first();
  await expect(rel).toContainText("The.Matrix");
  await rel.getByRole("button", { name: "Grab" }).click();
  await expect(page.locator(".drawer-note")).toContainText("Grabbed");

  // --- the immediate downloader imports it; the library reads "owned" on reload ---
  await expect(async () => {
    await page.goto("/movies");
    await expect(page.getByRole("button", { name: /Owned/ })).toContainText("1");
  }).toPass({ timeout: 40_000 });

  // --- the Overview renders ---
  await page.locator(".nav").getByRole("link", { name: "Overview" }).click();
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
});
