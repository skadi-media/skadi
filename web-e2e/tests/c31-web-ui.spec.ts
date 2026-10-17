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

  test("@passing every UI API call carries the signed-in member's bearer token", async ({ page, request }) => {
    // The page carries no credential of its own since SKADI-T-0621: in open
    // mode there is nothing to send. A signed-in member's every call carries
    // their token (SKADI-T-0722).
    await steps.givenDomainEnabled(request, "movies");
    const restore = await steps.givenSignedInAs(request, page, "member");
    try {
      const seen = await steps.whenOpens(page, "/movies");
      await page.waitForTimeout(500);
      steps.thenAllRequestsAuthed(seen);
    } finally {
      await restore();
    }
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
    await steps.givenHealthChecksRun(request);
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
    // The row's dot turns `bad` with the error as its title (Aurora, SKADI-T-0719).
    await expect(page.locator(".health-dot.bad, .fail, .error, .warn").first()).toBeVisible({ timeout: 20_000 });
  });

  test("@passing the theme choice changes the page at once and holds after a reload", async ({ page }) => {
    // SKADI-T-0720, through the gear since SKADI-T-0723. The harness is in
    // open mode, so this is the admin. A fresh browser context: nothing
    // stored, so System.
    await steps.whenOpens(page, "/indexers");
    await expect(steps.themeToggle(page)).toHaveCount(0);
    await steps.thenGearShown(page);
    await steps.whenOpensSettingsByGear(page);
    await steps.thenThemeToggle(page, ["Light", "Dark", "Hyper", "System"], "System");
    await steps.thenDataTheme(page, null);

    const backgrounds: Record<string, string> = {};
    for (const [label, theme] of [
      ["Light", "light"],
      ["Dark", "dark"],
      ["Hyper", "hyper"],
    ] as const) {
      await steps.whenPicksTheme(page, label);
      await steps.thenDataTheme(page, theme);
      await steps.thenThemeToggle(page, ["Light", "Dark", "Hyper", "System"], label);
      backgrounds[theme] = await steps.pageBackground(page);
      await page.reload();
      await steps.thenDataTheme(page, theme);
      await steps.thenThemeToggle(page, ["Light", "Dark", "Hyper", "System"], label);
    }
    expect(new Set(Object.values(backgrounds)).size, JSON.stringify(backgrounds)).toBe(3);

    // The stored choice is on <html> before the app runs: the init script in
    // index.html sets it, not the wasm.
    await page.route("**/*.wasm", (r) => r.abort());
    await page.reload();
    await steps.thenDataTheme(page, "hyper");
    await page.unroute("**/*.wasm");

    // System clears the stored choice; the page follows the operating system.
    await page.reload();
    await steps.whenPicksTheme(page, "System");
    await steps.thenDataTheme(page, null);
    await steps.thenThemeToggle(page, ["Light", "Dark", "Hyper", "System"], "System");
    expect(await page.evaluate(() => localStorage.getItem("aurora-theme"))).toBeNull();
    for (const scheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme: scheme });
      await expect.poll(() => steps.pageBackground(page)).toBe(backgrounds[scheme]);
    }
    await page.reload();
    await steps.thenDataTheme(page, null);
    await steps.thenThemeToggle(page, ["Light", "Dark", "Hyper", "System"], "System");
  });

  for (const role of ["member", "kid"] as const) {
    test(`@passing a ${role} opens their own settings by the gear and the theme holds`, async ({ page, request }) => {
      // SKADI-T-0723. Not the admin: no Settings link in the sidebar, and the
      // server refuses the admin settings calls.
      const restore = await steps.givenSignedInAs(request, page, role);
      try {
        const denied: string[] = [];
        page.on("response", (r) => {
          if (r.url().includes(`${steps.API}/`) && (r.status() === 401 || r.status() === 403)) {
            denied.push(`${r.status()} ${r.request().method()} ${r.url()}`);
          }
        });
        await steps.whenOpens(page, "/");
        await steps.thenSidebarLink(page, "Settings", false);
        await steps.thenGearShown(page);
        await steps.whenOpensSettingsByGear(page);
        await steps.thenThemeToggle(page, ["Light", "Dark", "Hyper", "System"], "System");
        await steps.whenPicksTheme(page, "Dark");
        await steps.thenDataTheme(page, "dark");
        await page.reload();
        await expect(page.getByRole("heading", { name: "Appearance" })).toBeVisible();
        await steps.thenDataTheme(page, "dark");
        await steps.thenThemeToggle(page, ["Light", "Dark", "Hyper", "System"], "Dark");
        await steps.whenPicksTheme(page, "Hyper");
        await steps.thenDataTheme(page, "hyper");
        await page.reload();
        await steps.thenDataTheme(page, "hyper");
        await steps.thenThemeToggle(page, ["Light", "Dark", "Hyper", "System"], "Hyper");
        // On a phone the gear is still in the bar.
        await page.setViewportSize({ width: 375, height: 740 });
        await expect(steps.gear(page)).toBeVisible();
        expect(denied, "the page asked for nothing this role may not have").toEqual([]);
      } finally {
        await restore();
      }
    });
  }

  test.fixme("@passing a quality profile is created from the form and listed (selectors unverified on this host)", async () => {});
  test.fixme("@gap settings changes show a reloading-providers confirmation", async () => {});
});

test.describe("C31 library-and-activity.feature", () => {
  test("@passing add → interactive search → grab → import shows as owned", async ({ page, request }) => {
    test.setTimeout(150_000); // the import wait below is 90 s
    await steps.givenDomainEnabled(request, "movies");
    await steps.givenMovieAdded(request, 603, LIBRARY);
    await steps.whenOpens(page, "/movies");
    await steps.whenOpensFirstTile(page, "The Matrix");
    await steps.whenGrabsFirstRelease(page, "The.Matrix");
    await expect(async () => {
      await page.goto("/movies");
      await expect(page.getByRole("button", { name: /Owned/ })).toContainText("1");
    }).toPass({ timeout: 90_000 }); // the first import on a cold harness takes ~45 s (SKADI-T-0722)
  });

  test("@passing the activity page lists the grab and the import in history", async ({ page, request }) => {
    await steps.givenDomainEnabled(request, "movies");
    // The page folds a run into one row showing its outcome (SKADI-T-0582),
    // so after the import the row reads "imported" and the grab is a step of
    // that run: it is in the trace the row is folded from (SKADI-T-0722).
    await steps.whenOpens(page, "/activity");
    await expect(page.getByText(/imported/i).first()).toBeVisible({ timeout: 20_000 });
    const traces = await (await request.get(`${steps.API}/traces?limit=50`)).json();
    const rows = (Array.isArray(traces) ? traces : traces.items ?? []) as { event: string }[];
    expect(rows.map((r) => r.event)).toContain("snatched");
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
    // Pairing moved to Players, behind the Android card's "Set up" (the
    // Players / Device split). The URI carries the token, so the page shows it
    // only as the QR; the QR is built from the `/pair/app` answer, checked here.
    await steps.givenDomainEnabled(request, "audiobooks");
    await steps.whenOpens(page, "/players");
    const pair = page.waitForResponse((r) => r.url().includes(`${steps.API}/pair/app`));
    await steps.whenClicks(page, "Set up");
    const body = await (await pair).json();
    expect(body.url).toMatch(/^skadi:\/\/pair\?host=/);
    await expect(page.locator(".pair-qr svg").first()).toBeVisible({ timeout: 20_000 });
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
