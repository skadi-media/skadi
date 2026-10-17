// Pages the Ralph loop (SKADI-T-0681 … T-0700) could only cover with wasm DOM
// tests, now in a live browser against the harness (SKADI-T-0713).
import { test, expect, Page } from "@playwright/test";
import * as path from "path";
import * as steps from "./steps";

const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, "..", ".tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");

/** A name no other test in the run seeds. */
const uniq = (base: string) => `${base}.${Date.now() % 100000}`;

test("System opens for the admin and is not offered to a member", async ({ page, request }) => {
  await steps.whenOpens(page, "/system");
  await expect(page.getByRole("heading", { name: "System" }).first()).toBeVisible();
  const restore = await steps.givenSignedInAs(request, page, "member");
  try {
    for (const p of ["/", "/preferences"]) {
      await steps.whenOpens(page, p);
      await expect(page.getByRole("link", { name: "System", exact: true })).toHaveCount(0);
    }
  } finally {
    await restore();
  }
});

test("the overview shows a failing check as a warning", async ({ page, request }) => {
  // The harness runs no download worker, so the worker check fails.
  await steps.givenHealthChecksRun(request);
  await steps.whenOpens(page, "/");
  await steps.thenHealthBadge(page, "worker", "bad");
});

test("Downloads: chips filter, badges explain, the queue moves, bulk selects, a failed import offers a retry", async ({ page, request }) => {
  const [stalled, errored, ok, failed, first, second] = [
    uniq("Bravo"), uniq("Charlie"), uniq("Alpha"), uniq("Hotel"), uniq("Echo"), uniq("Foxtrot"),
  ];
  await steps.givenDownloads(request, [
    { name: ok, state: "downloading" },
    { name: stalled, state: "stalled" },
    { name: errored, state: "error" },
    { name: failed, state: "seeding", target: ["movie", `ed-${failed}`], import_failed: "no file matched" },
    { name: first, state: "queued" },
    { name: second, state: "queued" },
  ]);
  await steps.whenOpens(page, "/downloaders");
  const row = (name: string) => page.locator("tbody tr", { hasText: name });
  await expect(row(ok)).toBeVisible();

  // The Stalled chip shows only stalled rows; the badge says for how long.
  await page.getByRole("button", { name: /^Stalled/ }).first().click();
  await expect(row(stalled)).toBeVisible();
  await expect(row(ok)).toHaveCount(0);
  await expect(row(stalled).locator(".dl-badge")).toHaveAttribute("title", /^Stalled for /);
  await page.getByRole("button", { name: /^All/ }).first().click();
  await expect(row(errored).locator(".dl-badge")).toHaveAttribute("title", "tracker said no");

  // The second queued row moves up to first.
  await expect(row(second)).toContainText("#2");
  // A plain click also checks the button gets it: the queue moves used to
  // spill under the ETA and Added cells, which took the click (SKADI-T-0713).
  await row(second).getByRole("button", { name: "Move up the queue" }).click();
  await expect(row(second)).toContainText("#1");

  // Selecting a row opens the bulk bar.
  await row(ok).getByRole("checkbox").check();
  await expect(page.locator(".dl-bulk-count")).toHaveText("1 selected");

  // A transfer whose import failed offers a retry.
  await page.getByRole("button", { name: /Seeding \(/ }).click();
  await expect(page.getByRole("button", { name: "Retry import…" }).first()).toBeVisible();
});

test("Activity lists an in-flight run with its row actions", async ({ page, request }) => {
  const name = uniq("Golf");
  await steps.givenDownloads(request, [
    { name, state: "downloading", target: ["movie", `ed-${name}`], run: true },
  ]);
  await steps.whenOpens(page, "/activity");
  for (const action of ["Pause", "Retry", "Remove", "Remove + blocklist"]) {
    await expect(page.getByRole("button", { name: action, exact: true }).first()).toBeVisible();
  }
});

test("a destructive action asks first, and Cancel keeps the item", async ({ page, request }) => {
  await steps.givenDomainEnabled(request, "movies");
  await steps.givenMovieAdded(request, 603, LIBRARY);
  await steps.whenOpens(page, "/movies");
  await steps.whenOpensFirstTile(page, "The Matrix");
  await page.getByRole("button", { name: "Delete" }).first().click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Delete the movie?");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toHaveCount(0);
  const movies = await (await request.get(`${steps.API}/movies`)).json();
  expect(movies.length, "Cancel deleted nothing").toBeGreaterThan(0);
});

test("the library sorts and selects in bulk", async ({ page, request }) => {
  await steps.givenDomainEnabled(request, "movies");
  await steps.givenMovieAdded(request, 603, LIBRARY);
  await steps.whenOpens(page, "/movies");
  const sort = page.getByLabel("Sort by");
  await sort.selectOption({ label: "Year" });
  await expect(sort).toHaveValue(/year/i);
  await page.getByRole("button", { name: "Select", exact: true }).click();
  await expect(page.locator(".lib-toolbar-bulk .bulk-count")).toHaveText("0 selected");
  await page.getByRole("button", { name: /^Select all/ }).click();
  await expect(page.locator(".lib-toolbar-bulk .bulk-count")).toHaveText("1 selected");
  await page.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("button", { name: "Select", exact: true })).toBeVisible();
});

test("at phone width no page scrolls sideways", async ({ page, request }) => {
  await steps.givenDomainEnabled(request, "movies");
  await page.setViewportSize({ width: 375, height: 800 });
  for (const p of ["/movies", "/activity", "/downloaders", "/indexers", "/importlists"]) {
    // Not `whenOpens`: it waits for the sidebar, which is a drawer here.
    await page.goto(p);
    await expect(page.locator(".cl-appshell")).toBeVisible();
    await page.waitForTimeout(800);
    const [scroll, client] = await page.evaluate(() => [
      document.documentElement.scrollWidth,
      document.documentElement.clientWidth,
    ]);
    expect(scroll, `${p} is ${scroll}px wide in a ${client}px window`).toBeLessThanOrEqual(client + 1);
  }
});

test("a slow load shows a skeleton; a failed one offers a Retry that works", async ({ page, request }) => {
  await steps.givenDomainEnabled(request, "movies");
  await steps.givenMovieAdded(request, 603, LIBRARY);
  const list = `**${steps.API}/movies`;
  await page.route(list, async (r) => {
    await new Promise((done) => setTimeout(done, 1500));
    await r.fallback();
  });
  await page.goto("/movies");
  await expect(page.locator(".skeleton").first()).toBeVisible();
  await expect(page.locator(".poster-tile").first()).toBeVisible();
  await page.unroute(list);

  await page.route(list, (r) => r.fulfill({ status: 500, body: "boom" }));
  await page.goto("/movies");
  await expect(page.getByText(/Load failed/)).toBeVisible();
  await page.unroute(list);
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(page.locator(".poster-tile").first()).toBeVisible();
});

test("an indexer card says when it was last tested", async ({ page, request }) => {
  const name = uniq("lasttest");
  await steps.givenTorznabIndexer(request, name);
  await steps.givenHealthChecksRun(request);
  await steps.whenOpens(page, "/indexers");
  const card = page.locator(".provider-card", { hasText: name });
  await expect(card).toContainText(/Last tested .*: failed/);
});

test("an import list is created, synced, edited and deleted; an item is excluded (SKADI-T-0733)", async ({ page }) => {
  const name = uniq("Bond");
  await steps.whenOpens(page, "/importlists");
  await expect(page.getByRole("heading", { name: "Import lists" })).toBeVisible();

  // The form says what is missing before it sends anything.
  await page.getByRole("button", { name: "Add list" }).click();
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Give the list a name.")).toBeVisible();
  await page.getByLabel("Name").fill(name);
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("TMDB id is required.")).toBeVisible();
  await page.getByLabel("TMDB id").fill("645");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText(`Saved ${name}.`)).toBeVisible();
  const row = page.locator(".import-list-row", { hasText: name });
  await expect(row).toContainText("Never synced");

  // The harness registers no TMDB provider, so a sync fails and says why, on
  // the row as well as at the top.
  await row.getByRole("button", { name: "Sync now" }).click();
  await expect(row).toContainText("no import-list provider for kind");
  await expect(row).not.toContainText("Validation error");

  // An edit opens with the stored settings, and a Sonarr list asks for its server.
  await row.getByRole("button", { name: "Edit" }).click();
  await expect(page.getByLabel("TMDB id")).toHaveValue("645");
  await page.getByLabel("Provider").selectOption("sonarr");
  await expect(page.getByLabel("Server URL")).toBeVisible();
  await expect(page.getByLabel("API key")).toBeVisible();
  await page.getByRole("button", { name: "Cancel" }).click();

  // An excluded item is listed with its id.
  const title = uniq("The Matrix");
  await page.getByLabel("Id", { exact: true }).fill("603");
  await page.getByLabel("Title (optional)").fill(title);
  await page.getByRole("button", { name: "Exclude" }).click();
  const excl = page.locator(".import-exclusion", { hasText: title });
  await expect(excl).toContainText("tmdb:603");
  await excl.getByRole("button", { name: "Remove" }).click();
  await expect(excl).toHaveCount(0);

  // Delete asks first.
  await row.getByRole("button", { name: "Delete" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText(`Deleted ${name}.`)).toBeVisible();
  await expect(row).toHaveCount(0);
});

test("a movie page excludes the movie from import lists; a member is not offered it (SKADI-T-0733)", async ({ page, request }) => {
  await steps.givenDomainEnabled(request, "movies");
  await steps.givenMovieAdded(request, 603, LIBRARY);
  await steps.whenOpens(page, "/movies");
  await steps.whenOpensFirstTile(page, "The Matrix");
  await page.getByRole("button", { name: "Exclude from lists" }).click();
  await expect(page.getByRole("button", { name: "Excluded from lists" })).toBeDisabled();
  const all = (await (await request.get(`${steps.API}/importlists/exclusions`)).json()) as {
    id: string; id_kind: string; external_id: string; title?: string;
  }[];
  const mine = all.filter((e) => e.id_kind === "tmdb" && e.external_id === "603");
  expect(mine.length, "the exclusion names the movie's TMDB id").toBeGreaterThan(0);
  expect(mine[0].title).toBe("The Matrix");
  for (const e of mine) await request.delete(`${steps.API}/importlists/exclusions/${e.id}`);

  const restore = await steps.givenSignedInAs(request, page, "member");
  try {
    await steps.whenOpens(page, "/movies");
    await steps.whenOpensFirstTile(page, "The Matrix");
    await expect(page.getByRole("button", { name: "Exclude from lists" })).toHaveCount(0);
  } finally {
    await restore();
  }
});
