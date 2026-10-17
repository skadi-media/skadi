---
title: Audiobooks
description: Enable the audiobooks domain, add indexers and books, and import a library.
---

Skadi's audiobooks domain is a full vertical alongside movies: an
**Author → Series → Book** library, keyless metadata from
[Audnexus](https://audnex.us) (ASIN-keyed), an audiobook-specific quality axis
(format + bitrate + abridgement), and the same acquire pipeline
(search → decide → snatch → monitor → import → notify) the movies domain uses.

## Enable the domain

The audiobooks domain ships disabled. Enable it once:

```bash
skadi domain enable audiobooks
```

or toggle it from the web UI (**Audiobooks → Config**).

## Prerequisites: a quality profile and a root folder

Audiobooks reuse the shared settings registry. Create at least one quality
profile and one root folder before adding books (the add flow refuses to bind a
book to nonexistent config):

```bash
# A permissive audiobook profile (allow all formats up to M4B-256).
skadi settings add profiles '{"name":"Audiobook Standard"}'
skadi settings add root_folders '{"path":"/audiobooks"}'
```

On-disk layout is **Author → [Series →] Book**, ASIN-tagged for exact
re-matching, no spaces:

```text
/audiobooks/Andy_Weir/Project_Hail_Mary_{asin-B08G9PRS1K}/Project_Hail_Mary.m4b
/audiobooks/Brandon_Sanderson/Stormlight_Archive/1_-_The_Way_of_Kings_{asin-…}/<file>.mp3
```

A single-file audiobook (one `.m4b`) is renamed to the title; a multi-file
audiobook (a folder of MP3s) keeps each source file's name.

## Indexers: AudiobookBay, built in

AudiobookBay (ABB) is in Skadi's bundled tracker library. The built-in
Cardigann engine searches it directly: no Jackett, Prowlarr or other Torznab
proxy is needed. Skadi searches audiobooks as Torznab **category 3030**
(Audio / Audiobook), and ABB serves that category.

1. Add the tracker. In the web UI, open **Settings → Indexers**, click
   **+ Add tracker (native)**, pick **AudioBook Bay** from the catalog and give
   it a name. Or from the CLI:

   ```bash
   skadi settings add indexers '{
     "kind": "cardigann",
     "name": "AudioBook Bay",
     "definition_id": "audiobookbay"
   }'
   ```

2. Test it: `skadi settings test indexers <id>` (or the **Test** button in the
   UI). Skadi only sends audiobook searches (category 3030) to indexers that
   advertise audiobook support, so a movies-only indexer is never queried for
   books and vice-versa.

Notes on ABB:

- It is a public tracker. It does not report seeders, so Skadi records a
  placeholder of 1 for each release.
- It needs no CloudFlare solver. In production every ABB fetch was served
  directly (SKADI-T-0552, 2026-09-10), so FlareSolverr is not involved.
- In the deploy stack the daemon's tracker searches leave through gluetun's
  HTTP proxy, so ABB sees the VPN exit address, not your home address. See the
  deploy README for the kill-switch wiring.

The download client is the same one movies and TV use — the built-in worker,
registered once in the downloaders settings.

## Add a book by ASIN

Audnexus is **ASIN-keyed and has no title search**, so books are added by their
Audible ASIN (the `B0…` id in an Audible URL). Paste the ASIN in the web UI
(**Audiobooks → Add**) for a metadata preview, or:

```bash
skadi audiobook add --asin B08G9PRS1K
```

Skadi fetches the Audnexus record (title, authors, narrators, series + position,
cover, runtime), writes the `Book`, and creates one `Missing` book file the
hunter then acquires. Add `--no-search` to register without an immediate search.

## Author monitoring (auto-discovery)

Mark an author **monitored** and Skadi periodically discovers their new releases
and adds them automatically (as monitored, `Missing` books the hunter acquires).

Because Audnexus has no "list books by author" endpoint, discovery lists the
author's catalog from the **Audible catalog API**
(`api.audible.com/1.0/catalog/products?author=…`, keyless — the same upstream
Audnexus itself derives from) and enriches each new ASIN through Audnexus.

Toggle monitoring from an author's detail page, or when adding the author.

## Importing an existing library

Already have audiobooks on disk? Use **library import**
(**Audiobooks → Import**, or the `/audiobooks/library-import/*` API):

1. **Scan** a path — Skadi walks the tree, parses author/title/series, and
   extracts the ASIN when a folder is ASIN-tagged.
2. **Review** — correct or supply the Audible ASIN for any unmatched item
   (matching is ASIN-driven).
3. **Commit** — selected items are fetched from Audnexus and **hardlinked** into
   the canonical layout as `Imported`. Originals are never modified or removed.

## Convert downloads to M4B (optional)

Skadi prefers an M4B release when it searches. When only MP3 exists, it can
convert the download to one M4B file with chapters after the import. This is
off by default: MP3 to AAC is a lossy re-encode.

Turn it on with the config key `import.audiobook_convert_to_m4b` (**Config →
import**), or `SKADI_IMPORT_AUDIOBOOK_CONVERT_TO_M4B=true` in the environment.
The deploy image carries a small static `ffmpeg` for this job and nothing else.

- **Which books.** Books that Skadi downloads and imports as something other
  than M4B. A library import is never converted.
- **When.** A background job picks the book up shortly after the import, one
  book at a time. It shows in Activity at the stage `converting`, and its
  result is on the book's trace stream (`converted`, `convert_failed`,
  `convert_skipped`).
- **Chapters.** From a `.cue` sheet next to the files when it fits them, else
  one chapter per file (named from the file name), else the file's own
  chapters. A book without chapters gets one.
- **Audio.** AAC at 128 kbps for a source at 112 kbps or more, else 64 kbps.
  M4A/MP4 sources are copied, not re-encoded. The file is recorded at the
  matching M4B quality tier.
- **Failure.** The imported files stay as they were. A failed book is tried
  again after a day, at most three times.
- **Seeding stops.** Once the M4B is recorded, the book's download is removed
  with its data, and every original in the library goes: to the recycle bin
  when `import.recycle_bin` is set, else it is deleted. Skadi does not seed the
  M4B.

The conversion needs free space for the new file next to the originals.

## Metadata refresh

Once added, a book's Audnexus-sourced fields are refreshed on a schedule (behind
a per-provider circuit breaker, so a failing upstream isn't hammered). Your
user-controlled fields (monitored state, profile, root) are preserved.
