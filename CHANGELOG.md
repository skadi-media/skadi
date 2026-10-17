# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Changed
- Web client on Aurora 0.6.1. Skadi's drawer, scroll lock, `<main>` id and `#main` height workarounds are gone; Skadi keeps its own skip link.
- Docs: plissken 0.2.0 (the release binary, pinned by sha256). Private items are out of the API reference; it has an overview page.
- Docs theme starlight-aurora v0.3.0.
- Docs: plissken 0.2.1 (was 0.2.0): intra-doc links resolve by the whole path (SKADI-T-0727).

### Added
- Optional conversion of a downloaded audiobook to one M4B with chapters (`import.audiobook_convert_to_m4b`, off by default). The image now has a small static `ffmpeg` for it (SKADI-T-0139).
- Initial project structure
