# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Fixed
- A multi-file audiobook reached the phone and the web player as its first part only. `/audio` now serves every MP3 part as one stream, `/chapters` lists the parts, the probed duration is the whole book, and a delete with files removes every part (SKADI-T-0738).

### Changed
- Web client on Aurora 0.6.1. Skadi's drawer, scroll lock, `<main>` id and `#main` height workarounds are gone; Skadi keeps its own skip link.
- Docs: plissken 0.2.0 (the release binary, pinned by sha256). Private items are out of the API reference; it has an overview page.
- Docs theme starlight-aurora v0.3.0.
- Docs: plissken 0.2.1 (was 0.2.0): intra-doc links resolve by the whole path (SKADI-T-0727).

### Added
- Initial project structure
