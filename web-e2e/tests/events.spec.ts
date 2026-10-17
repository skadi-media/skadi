// Server push for Downloads (SKADI-T-0711): the page reads the daemon's event
// stream and polls only when the stream is down.
import { test, expect, Request } from "@playwright/test";
import * as steps from "./steps";

const uniq = (base: string) => `${base}.${Date.now() % 100000}`;
const isPoll = (r: Request) => r.method() === "GET" && /\/api\/v1\/downloads(\?|$)/.test(r.url());
const isStream = (r: Request) => r.url().includes("/api/v1/events");

test("a change reaches the Downloads page by server push, with no client poll", async ({ page, request }) => {
  const opened = page.waitForRequest(isStream);
  await steps.whenOpens(page, "/downloaders");
  await opened;
  await page.waitForTimeout(2500); // past any first poll
  const polls: string[] = [];
  page.on("request", (r) => { if (isPoll(r)) polls.push(r.url()); });
  const name = uniq("Pushed");
  await steps.givenDownloads(request, [{ name, state: "downloading" }]);
  await expect(page.locator("tbody tr", { hasText: name })).toBeVisible({ timeout: 3_000 });
  expect(polls, "the row came by the stream, not a poll").toEqual([]);
});

test("a refused stream falls back to the poll, and the stream comes back", async ({ page, request }) => {
  await page.route("**/api/v1/events**", (r) => r.abort());
  const polled = page.waitForRequest(isPoll);
  await steps.whenOpens(page, "/downloaders");
  await polled;
  const name = uniq("Polled");
  await steps.givenDownloads(request, [{ name, state: "downloading" }]);
  await expect(page.locator("tbody tr", { hasText: name })).toBeVisible({ timeout: 6_000 });

  // Let the stream through: the page reconnects within the retry wait and
  // stops polling.
  await page.unroute("**/api/v1/events**");
  await page.waitForRequest(isStream, { timeout: 35_000 });
  await page.waitForTimeout(1500);
  const polls: string[] = [];
  page.on("request", (r) => { if (isPoll(r)) polls.push(r.url()); });
  const later = uniq("Pushed");
  await steps.givenDownloads(request, [{ name: later, state: "downloading" }]);
  await expect(page.locator("tbody tr", { hasText: later })).toBeVisible({ timeout: 3_000 });
  expect(polls, "back on the stream").toEqual([]);
});
