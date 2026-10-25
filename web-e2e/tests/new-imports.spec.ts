// New imports on the web (SKADI-T-0745): the Overview widget, "See all", the
// media filter, the Upgrade pill, a row that opens its item, and a kid who
// sees only what their policy allows.
import { test, expect, type APIRequestContext } from "@playwright/test";
import * as path from "path";
import * as steps from "./steps";

const API = steps.API;
const WORKDIR = process.env.SKADI_E2E_WORKDIR || path.resolve(__dirname, "..", ".tmp-e2e");
const LIBRARY = path.join(WORKDIR, "library");
const ASIN = "B08G9PRS1K";

/** The Matrix's edition and a fresh book file, each with an import. */
async function givenAFilmAndABookImported(request: APIRequestContext) {
  await steps.givenMovieAdded(request, 603, LIBRARY);
  const movies = (await (await request.get(`${API}/movies`)).json()) as {
    id: string;
    title: string;
    editions: { id: string }[];
  }[];
  const matrix = movies.find((m) => m.title === "The Matrix");
  expect(matrix, "The Matrix is in the library").toBeTruthy();

  expect(
    (await request.put(`${API}/domains/audiobooks`, { data: { enabled: true } })).ok(),
  ).toBeTruthy();
  let books = (await (await request.get(`${API}/books`)).json()) as {
    id: string;
    title: string;
    files: { id: string }[];
  }[];
  if (!books.length) {
    const profile = await request.post(`${API}/settings/profiles`, {
      data: { name: "e2e-books", cutoff: "M4B" },
    });
    const r = await request.post(`${API}/books`, {
      data: { asin: ASIN, profile: (await profile.json()).id, root: LIBRARY, search: false },
    });
    expect(r.ok(), `add book: ${r.status()}`).toBeTruthy();
    books = await (await request.get(`${API}/books`)).json();
  }
  const book = books[0];
  expect(book.files.length, "the book has a file row").toBeGreaterThan(0);

  await steps.givenImports(request, [
    { kind: "audiobook", acquirable_ref: book.files[0].id, label: book.title, minutes_ago: 2 },
    {
      kind: "movie",
      acquirable_ref: matrix!.editions[0].id,
      label: "The Matrix",
      upgrade: true,
      minutes_ago: 1,
    },
  ]);
  return { movieId: matrix!.id, bookTitle: book.title };
}

test("the overview lists new imports and See all filters them by media", async ({
  page,
  request,
}) => {
  const { movieId, bookTitle } = await givenAFilmAndABookImported(request);

  await steps.whenOpens(page, "/");
  const widget = page.getByRole("list", { name: "New imports" });
  await expect(widget).toContainText("The Matrix");
  await expect(widget).toContainText(bookTitle);
  const matrixRow = widget.getByRole("link").filter({ hasText: "The Matrix" }).first();
  await expect(matrixRow).toContainText("Upgrade");
  await expect(page.getByText(/new this week/)).toBeVisible();

  await page.getByRole("link", { name: "See all" }).click();
  await expect(page).toHaveURL(/\/new$/);
  const list = page.getByRole("list", { name: "New imports" });
  await expect(list).toContainText("The Matrix");
  await expect(list).toContainText(bookTitle);

  await page.getByLabel("Media").selectOption("movie");
  await expect(list).toContainText("The Matrix");
  await expect(list).not.toContainText(bookTitle);

  await list.getByRole("link").filter({ hasText: "The Matrix" }).first().click();
  await expect(page).toHaveURL(new RegExp(`/movies/${movieId}$`));
});

test("a kid's new imports leave out the book their policy hides", async ({ page, request }) => {
  const { bookTitle } = await givenAFilmAndABookImported(request);
  // A kid sees a book only when it is on their allowed list.
  const restore = await steps.givenSignedInAs(request, page, "kid");
  try {
    await steps.whenOpens(page, "/");
    const widget = page.getByRole("list", { name: "New imports" });
    await expect(widget).toContainText("The Matrix");
    await expect(widget).not.toContainText(bookTitle);
    await steps.whenOpens(page, "/new");
    const list = page.getByRole("list", { name: "New imports" });
    await expect(list).toContainText("The Matrix");
    await expect(list).not.toContainText(bookTitle);
  } finally {
    await restore();
  }
});
