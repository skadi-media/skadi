# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [0.1.9] - 2026-10-17

### Fixed
- A multi-file audiobook reached the phone and the web player as its first part only. `/audio` now serves every MP3 part as one stream, `/chapters` lists the parts, the probed duration is the whole book, and a delete with files removes every part (SKADI-T-0738).

### Changed
- Web client on Aurora 0.6.1. Skadi's drawer, scroll lock, `<main>` id and `#main` height workarounds are gone; Skadi keeps its own skip link.
- Docs: plissken 0.2.0 (the release binary, pinned by sha256). Private items are out of the API reference; it has an overview page.
- Docs theme starlight-aurora v0.3.0.
- Docs: plissken 0.2.1 (was 0.2.0): intra-doc links resolve by the whole path (SKADI-T-0727).

### Added
- Import lists in the web UI: a Lists page under Settings creates, edits, syncs and deletes lists, and keeps exclusions; a movie or series page can exclude itself from every list (SKADI-T-0733).
- IMDb lists, fetched through FlareSolverr, and audiobook lists, added monitored by default (SKADI-T-0733).
- Android: films and episodes download for offline play, the way audiobooks do (SKADI-T-0637).
- A per-role sweep in the web e2e suite; `angreal version bump` and the release jobs stop when a page offers a role what the API refuses (SKADI-T-0639).
- List records carry sort keys (`sort_title`, `sort_name`, `sort_authors`, `sort_series`, `sort_collection`); the web and Android order by them (SKADI-T-0737).
- Optional conversion of a downloaded audiobook to one M4B with chapters (`import.audiobook_convert_to_m4b`, off by default). The image now has a small static `ffmpeg` for it. A converted book's download stops seeding and its MP3s are removed (SKADI-T-0139).
- Initial project structure
