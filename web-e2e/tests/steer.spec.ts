import { test, expect } from "@playwright/test";
import * as path from "path";
import * as steps from "./steps";

// The harness writes its library + fixture under WORKDIR (shared via env with
// playwright.config.ts). The spec points a root folder at <WORKDIR>/library so
// the grabbed fixture imports there.
const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, "..", ".tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");
const API = "/api/v1";

test("steer-it: add → search → grab → import → dashboard", async ({
  page,
  request,
}) => {
  test.setTimeout(150_000); // the import wait below is 90 s
  // --- setup via API (plumbing, not the thing under test) ---
  expect(
    (await request.put(`${API}/domains/movies`, { data: { enabled: true } })).ok(),
  ).toBeTruthy();
  // A fresh copy of the film under LIBRARY (an earlier spec may have added
  // it), search:false so the only acquire is the manual grab below.
  await steps.givenMovieAdded(request, 603, LIBRARY);

  // --- open the detail page from a library tile ---
  await page.goto("/");
  await steps.sidebarLink(page, "Movies").click();
  await steps.whenOpensFirstTile(page, "The Matrix");

  // --- interactive search lists the mock release; grab it ---
  await steps.whenGrabsFirstRelease(page, "The.Matrix");

  // --- the immediate downloader imports it; the library reads "owned" on reload ---
  await expect(async () => {
    await page.goto("/movies");
    await expect(page.getByRole("button", { name: /Owned/ })).toContainText("1");
  }).toPass({ timeout: 90_000 }); // the first import on a cold harness takes ~45 s (SKADI-T-0722)

  // --- the Overview renders ---
  await steps.sidebarLink(page, "Overview").click();
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
});
