// Executable form of crates/skadi-web/tests/features/C31-web-ui/*.feature
// (SKADI-I-0057 pass P6). @passing scenarios are plain tests, @gap scenarios are
// `test.fixme` (skipped: the behaviour does not exist yet), @bug scenarios are
// `test.fail` (must currently fail; turn green when the bug is fixed).
import { test, expect } from "@playwright/test";
import * as path from "path";
import * as steps from "./steps";

const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, "..", ".tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");

test.describe("C31 shell-and-auth.feature", () => {
  test("@passing the shell loads and the sidebar lists the enabled domains only", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.whenOpens(page, "/");
    await steps.thenSidebarLink(page, "Overview");
    await steps.thenSidebarLink(page, "Movies");
    await steps.thenSidebarLink(page, "Television", false);
  });

  test("@passing every UI API call carries the injected bearer token", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    const seen = await steps.whenOpens(page, "/movies");
    await page.waitForTimeout(500);
    steps.thenAllRequestsAuthed(seen);
  });

  test("@passing an unknown client-side route renders the shell, not a daemon 404", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.whenOpens(page, "/no/such/page");
    await steps.thenSidebarLink(page, "Overview");
  });

  test.fixme("@gap an expired or rotated token is surfaced as a re-authenticate prompt", async () => {});
});

test.describe("C31 dashboard-health.feature", () => {
  test("@passing health badges render one row per check with a status dot", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.whenOpens(page, "/");
    await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
    await steps.thenHealthBadge(page, "daemon", "ok");
    await steps.thenHealthBadge(page, "database", "ok");
  });

  test("@passing the download worker and VPN panels degrade gracefully when absent", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.whenOpens(page, "/downloaders");
    await expect(page.getByRole("heading", { name: "Downloads" })).toBeVisible();
    await steps.thenNoErrorDialog(page);
  });

  test.fixme("@gap a library-root problem is visible on the Overview (SKADI-T-0430)", async () => {});
});

test.describe("C31 settings.feature", () => {
  test("@passing an indexer's api key is never echoed back into the edit form", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.givenTorznabIndexer(request, "nzbgeek");
    await steps.whenOpens(page, "/indexers");
    await expect(page.getByText("nzbgeek").first()).toBeVisible();
    // No input on the page holds the secret value we stored.
    const values = await page.locator("input").evaluateAll((els) => els.map((e) => (e as HTMLInputElement).value));
    expect(values).not.toContain("k");
  });

  test("@passing testing a dead indexer reports the failure inline", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.givenTorznabIndexer(request, "dead");
    await steps.whenOpens(page, "/indexers");
    await expect(page.getByText("dead").first()).toBeVisible();
    await steps.whenClicks(page, "Test");
    await expect(page.locator(".fail, .error, .warn, .health-dot.fail").first()).toBeVisible({ timeout: 20_000 });
  });

  test.fixme("@passing a quality profile is created from the form and listed (selectors unverified on this host)", async () => {});
  test.fixme("@gap settings changes show a reloading-providers confirmation", async () => {});
});

test.describe("C31 library-and-activity.feature", () => {
  test("@passing add → interactive search → grab → import shows as owned", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.givenMovieAdded(request, 603, LIBRARY);
    await steps.whenOpens(page, "/movies");
    await expect(page.locator(".poster-tile").first()).toBeVisible();
    await page.locator(".poster-tile").first().click();
    await expect(page.locator(".drawer-title")).toContainText("The Matrix");
    await steps.whenClicks(page, /Search releases/);
    const rel = page.locator(".drawer-release").first();
    await expect(rel).toContainText("The.Matrix");
    await rel.getByRole("button", { name: "Grab" }).click();
    await expect(page.locator(".drawer-note")).toContainText("Grabbed");
    await expect(async () => {
      await page.goto("/movies");
      await expect(page.getByRole("button", { name: /Owned/ })).toContainText("1");
    }).toPass({ timeout: 40_000 });
  });

  test("@passing the activity page lists the grab and the import in history", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.whenOpens(page, "/activity");
    await expect(page.getByText(/grabbed/i).first()).toBeVisible({ timeout: 20_000 });
    await expect(page.getByText(/imported/i).first()).toBeVisible({ timeout: 20_000 });
  });

  test("@passing the Add page searches the metadata provider and offers Add", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    await steps.whenOpens(page, "/add");
    await page.getByRole("textbox").first().fill("Matrix");
    await page.keyboard.press("Enter");
    await expect(page.getByText("The Matrix").first()).toBeVisible({ timeout: 20_000 });
    await expect(page.getByRole("button", { name: /Add/ }).first()).toBeVisible();
  });

  test.fixme("@gap the library grid pages instead of loading every item", async () => {});
  test.fixme("@gap bulk edit of monitored state across selected items", async () => {});
});

test.describe("C31 pairing.feature", () => {
  test("@passing the pairing card renders a QR that encodes skadi://pair with the token", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "audiobooks");
    await steps.whenOpens(page, "/listen");
    await expect(page.locator(".pair-qr svg").first()).toBeVisible({ timeout: 20_000 });
    await expect(page.getByText(/skadi:\/\/pair\?host=/).first()).toBeVisible();
  });

  test("@passing no published APK means no install link, not an error", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "audiobooks");
    await steps.whenOpens(page, "/listen");
    await expect(page.getByRole("link", { name: /\.apk/ })).toHaveCount(0);
    await steps.thenNoErrorDialog(page);
  });
});

test.describe("C31 e2e-suite-drift.feature", () => {
  test("the steer-it E2E spec seeds a root folder the API accepts", async ({ request }) => {
    // Mirrors the seed in steer.spec.ts / audiobooks.spec.ts. The single library
    // root is the config key `library.root` (SKADI-T-0302); the `root_folders`
    // settings kind it replaced is gone, and seeding it 404'd both suites at
    // their first expect (SKADI-T-0461).
    const r = await request.put(`${steps.API}/config/library.root`, { data: { value: LIBRARY } });
    expect(r.ok(), `PUT /config/library.root -> ${r.status()}`).toBeTruthy();
  });

  test("the removed settings kind really is gone", async ({ request }) => {
    // The other half of the drift check: if `root_folders` ever came back, the
    // seed above would be silently redundant rather than wrong, and this suite
    // would stop being able to tell the two apart.
    const r = await request.post(`${steps.API}/settings/root_folders`, { data: { path: LIBRARY } });
    expect(r.status(), "root_folders was removed by SKADI-T-0302").toBe(404);
  });
});
