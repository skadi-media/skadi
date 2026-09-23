# Audiobooks

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

## Indexers: AudiobookBay via Jackett / Prowlarr (Torznab)

Skadi has **no native AudiobookBay scraper**. Instead, run AudiobookBay (ABB)
through a Torznab proxy — [Jackett](https://github.com/Jackett/Jackett) or
[Prowlarr](https://prowlarr.com) — and point Skadi at the proxy's Torznab feed.
Skadi searches audiobooks as Torznab **category 3030** (Audio / Audiobook).

1. In Jackett/Prowlarr, add the **AudiobookBay** indexer (Jackett ships an ABB
   definition; in Prowlarr add it from the indexer catalog).
2. Copy that indexer's **Torznab feed URL** and **API key**.
3. Register it in Skadi as a `torznab` indexer:

   ```bash
   skadi settings add indexers '{
     "kind": "torznab",
     "name": "AudiobookBay (Jackett)",
     "url": "http://127.0.0.1:9117/api/v2.0/indexers/audiobookbay/results/torznab/",
     "api_key": "<jackett-or-prowlarr-api-key>"
   }'
   ```

   (Prowlarr exposes a single aggregate Torznab endpoint
   `http://127.0.0.1:9696/<n>/api` plus its API key; either works.)
4. Test it: `skadi settings test indexers <id>` (or the **Test** button in the
   UI). Skadi only sends audiobook searches (category 3030) to indexers that
   advertise audiobook support, so a movies-only indexer is never queried for
   books and vice-versa.

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
Audnexus itself derives from) and enriches each new ASIN through Audnexus. See
[ADR&nbsp;SKADI-A-0001] for the keyless/private-use posture that governs both
endpoints.

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

## Metadata refresh

Once added, a book's Audnexus-sourced fields are refreshed on a schedule (behind
a per-provider circuit breaker, so a failing upstream isn't hammered). Your
user-controlled fields (monitored state, profile, root) are preserved.

[ADR&nbsp;SKADI-A-0001]: ../index.md
