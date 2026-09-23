//! The **audiobook** quality axis (SKADI-I-0017 / SKADI-T-0121).
//!
//! Audiobooks don't have resolution/source/codec the way video does — their
//! quality is **format** (container/codec) + **bitrate** + **abridgement**. This
//! module is the audiobook-shaped parallel to [`crate::quality`]: typed
//! dimensions, a ranked set of [`AudiobookQualityDefinition`]s sharing the same
//! [`QualityId`] space as movie definitions, and [`to_audiobook_quality`] to map
//! a parsed release onto one.
//!
//! A [`QualityProfile`](crate::profile::QualityProfile) is axis-agnostic — it
//! ranks/gates by `QualityId` (`allowed`/`cutoff`) via
//! [`decide_id`](crate::profile::QualityProfile::decide_id) — so an audiobook
//! profile is just one whose `allowed`/`cutoff` reference these definition ids.
//! Clean-room from observable release-naming conventions.

use serde::{Deserialize, Serialize};
use skadi_core::QualityId;
use uuid::Uuid;

use crate::parsed::ParsedRelease;

/// Audiobook container/codec format. `Ord` follows declaration order (used only
/// as a deterministic tie-break; real preference comes from the profile's ranked
/// `allowed` list, like movie qualities).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AudioFormat {
    Mp3,
    Aac,
    Ogg,
    Opus,
    M4a,
    M4b,
    Flac,
}

impl AudioFormat {
    /// Map a release/file token (case-insensitive, leading `.` tolerated).
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().trim_start_matches('.') {
            "m4b" => Some(Self::M4b),
            "m4a" | "mp4" => Some(Self::M4a),
            "mp3" => Some(Self::Mp3),
            "aac" => Some(Self::Aac),
            "flac" => Some(Self::Flac),
            "ogg" => Some(Self::Ogg),
            "opus" => Some(Self::Opus),
            _ => None,
        }
    }
}

/// Audiobook bitrate, bucketed into tiers and ordered low → high. `Vbr` (nominal
/// rate unknown) sorts lowest so it never out-ranks a known CBR tier.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AudioBitrate {
    Vbr,
    Kbps32,
    Kbps64,
    Kbps96,
    Kbps128,
    Kbps192,
    Kbps256,
}

impl AudioBitrate {
    /// Bucket a nominal kbps value into a tier.
    #[must_use]
    pub fn from_kbps(kbps: u32) -> Self {
        match kbps {
            0..=47 => Self::Kbps32,
            48..=79 => Self::Kbps64,
            80..=111 => Self::Kbps96,
            112..=159 => Self::Kbps128,
            160..=223 => Self::Kbps192,
            _ => Self::Kbps256,
        }
    }

    /// Map a token like `128kbps`, `64`, or `vbr` to a tier.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        let t = token.to_ascii_lowercase();
        if t == "vbr" {
            return Some(Self::Vbr);
        }
        let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u32>().ok().map(Self::from_kbps)
    }
}

/// Whether an audiobook release is the full (unabridged) text or a cut
/// (abridged) reading. Default policy rejects abridged unless a profile allows
/// it (the abridged-reject floor lives in the hunter's scoring step).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Abridgement {
    Unabridged,
    Abridged,
}

impl Abridgement {
    /// Map a token to an abridgement signal, if it carries one.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "unabridged" => Some(Self::Unabridged),
            "abridged" => Some(Self::Abridged),
            _ => None,
        }
    }
}

/// A named, stable audiobook quality definition — the unit an audiobook
/// [`QualityProfile`](crate::profile::QualityProfile) orders and selects from.
/// Shares the `QualityId` space with movie [`QualityDefinition`](crate::quality::QualityDefinition).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudiobookQualityDefinition {
    pub id: QualityId,
    pub name: String,
    pub format: AudioFormat,
    pub bitrate: AudioBitrate,
}

/// Deterministic `QualityId` for built-in audiobook definitions. Offset by 1000
/// so the ids never collide with the movie definitions (`Uuid::from_u128(1..)`).
fn def_id(n: u128) -> QualityId {
    QualityId(Uuid::from_u128(1000 + n))
}

/// The sentinel **Unknown** audiobook quality (SKADI-T-0175): an accepted-but-
/// unspecced release — its title carries no parseable format+bitrate, as on
/// indexers that tag only `(Audiobook)` (e.g. 1337x). Ranked **lowest** so any
/// properly-tagged release outranks it; it stays "Unknown" through import (we
/// don't read the file's bitrate). Its placeholder format/bitrate in the ladder
/// are never matched directly — [`to_audiobook_quality`] only returns it as a
/// fallback, and [`reconcile_quality_with_format`] leaves it untouched.
#[must_use]
pub fn unknown_audiobook_id() -> QualityId {
    def_id(0)
}

/// The baseline ranked list of audiobook quality definitions (low → high),
/// re-derived clean-room. Profiles select an ordered subset of these.
///
/// **Format-first ranking (operator policy):** every **M4B** tier outranks every
/// **MP3** tier — a single-file M4B is the premium audiobook container, so any
/// M4B is preferred over any MP3 (even `M4B-64` over `MP3-128`); MP3 stays
/// available as a fallback when no M4B release exists. The list order *is* the
/// rank ([`QualityProfile::rank`](crate::QualityProfile) = index in `allowed`),
/// so the rows below are ordered MP3s-then-M4Bs. The per-definition id
/// ([`def_id`]) is decoupled from position and stays stable, so reordering the
/// ladder never changes an already-classified file's `QualityId`.
#[must_use]
pub fn default_audiobook_definitions() -> Vec<AudiobookQualityDefinition> {
    use AudioBitrate::*;
    use AudioFormat::*;
    // (id, name, format, bitrate) — listed low → high. `id` is stable identity;
    // the *position* is the rank. Unknown sits at the very bottom (placeholder
    // format/bitrate — see `unknown_audiobook_id`); all MP3 tiers above it, all
    // M4B tiers above those.
    let rows: &[(u128, &str, AudioFormat, AudioBitrate)] = &[
        (0, "Unknown", Mp3, Vbr),
        (1, "MP3-32", Mp3, Kbps32),
        (2, "MP3-64", Mp3, Kbps64),
        (4, "MP3-128", Mp3, Kbps128),
        (3, "M4B-64", M4b, Kbps64),
        (5, "M4B-128", M4b, Kbps128),
        (6, "M4B-192", M4b, Kbps192),
        (7, "M4B-256", M4b, Kbps256),
    ];
    rows.iter()
        .map(|(n, name, format, bitrate)| AudiobookQualityDefinition {
            id: def_id(*n),
            name: (*name).to_string(),
            format: *format,
            bitrate: *bitrate,
        })
        .collect()
}

/// Map a parsed audiobook release to a definition id. A title that states a
/// modeled format + bitrate maps to that exact tier. A title that doesn't — no
/// format/bitrate at all (e.g. `(Audiobook)`), or a format with no modeled tier
/// (e.g. FLAC) — falls back to the **Unknown** tier (lowest rank) so it's still
/// acquirable rather than silently dropped (SKADI-T-0175). Returns `None` only
/// when the definition set carries no Unknown tier (e.g. a custom profile).
#[must_use]
pub fn to_audiobook_quality(
    parsed: &ParsedRelease,
    definitions: &[AudiobookQualityDefinition],
) -> Option<QualityId> {
    let unknown = unknown_audiobook_id();
    let format = parsed
        .audio_format
        .as_deref()
        .and_then(AudioFormat::from_token);
    let bitrate = parsed.bitrate_kbps.map(AudioBitrate::from_kbps);
    // Exact tier match (never the sentinel — its placeholder format/bitrate must
    // not catch a real release).
    if let (Some(format), Some(bitrate)) = (format, bitrate)
        && let Some(d) = definitions
            .iter()
            .find(|d| d.id != unknown && d.format == format && d.bitrate == bitrate)
    {
        return Some(d.id);
    }
    // Unspecced / unmodeled → Unknown, if the profile carries it.
    definitions.iter().find(|d| d.id == unknown).map(|d| d.id)
}

/// Reconcile a release-title-derived quality against the **actual file's**
/// container format (SKADI-T-0148). The bytes are ground truth: when the file's
/// format (from its extension) disagrees with the parsed quality's format — e.g.
/// a release advertised as `M4B-256` whose contents are MP3 — switch to the
/// closest tier of the file's format: the same bitrate if it's modeled, else the
/// highest tier at-or-below the parsed bitrate, else the lowest. Returns `parsed`
/// unchanged when the formats already agree or the file format isn't modeled.
#[must_use]
pub fn reconcile_quality_with_format(
    parsed: Option<QualityId>,
    file_format: AudioFormat,
    definitions: &[AudiobookQualityDefinition],
) -> Option<QualityId> {
    // Unknown stays Unknown — we never inferred a format/bitrate to correct, and
    // we don't read the file's bitrate, so guessing a tier would be dishonest
    // (SKADI-T-0175).
    if parsed == Some(unknown_audiobook_id()) {
        return parsed;
    }
    let parsed_def = parsed.and_then(|id| definitions.iter().find(|d| d.id == id));
    if parsed_def.map(|d| d.format) == Some(file_format) {
        return parsed; // already consistent with the bytes
    }
    // Candidates of the file's actual format, in ladder order (low → high). The
    // Unknown sentinel is excluded — reconcile only ever maps to a real tier.
    let unknown = unknown_audiobook_id();
    let same_format: Vec<&AudiobookQualityDefinition> = definitions
        .iter()
        .filter(|d| d.id != unknown && d.format == file_format)
        .collect();
    if same_format.is_empty() {
        return parsed; // file format not modeled — keep what we had
    }
    match parsed_def.map(|d| d.bitrate) {
        // Highest tier of this format at or below the parsed bitrate, else lowest.
        Some(bt) => same_format
            .iter()
            .rfind(|d| d.bitrate <= bt)
            .or_else(|| same_format.first())
            .map(|d| d.id),
        None => same_format.first().map(|d| d.id),
    }
}

/// Heuristic: does this release look like an **ebook** rather than an audiobook?
/// skadi now searches the Books/EBook categories too (SKADI-T-0176), which surface
/// the ebook edition of a title; and since [`to_audiobook_quality`] accepts
/// format-less releases at the Unknown tier (SKADI-T-0175), an ebook would
/// otherwise slip through. The `Release` carries no category at filter time, so we
/// discriminate on the title + size:
///
/// 1. Any **audio signal** — an audio-format token (`m4b`/`mp3`/…) or an
///    `audiobook`/`unabridged` keyword — means it's an audiobook (incl. combined
///    audiobook+ebook packs): never an ebook.
/// 2. Otherwise, **too small** to be a full audiobook (< ~10 MiB) ⇒ ebook/sample.
/// 3. Otherwise, an explicit **ebook-format token** (`epub`/`mobi`/`azw3`/`pdf`/…)
///    ⇒ ebook.
/// 4. Otherwise (format-less, reasonably sized) ⇒ treat as an audiobook (Unknown).
#[must_use]
pub fn looks_like_ebook(title: &str, size_bytes: u64) -> bool {
    /// Below this, a release can't be a full audiobook (a 20-min reading at 64 kbps
    /// is ~10 MiB); ebooks are well under it.
    const MIN_AUDIOBOOK_BYTES: u64 = 10 * 1024 * 1024;
    const AUDIO_SIGNALS: &[&str] = &[
        "audiobook",
        "unabridged",
        "m4b",
        "mp3",
        "m4a",
        "aac",
        "flac",
        "opus",
    ];
    const EBOOK_FORMATS: &[&str] = &[
        "epub", "mobi", "azw3", "azw", "pdf", "cbr", "cbz", "fb2", "djvu", "lit",
    ];

    let lower = title.to_ascii_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    // Whole-token match for the audio signals too (SKADI-T-0595 follow-up):
    // as a substring test, "Isaac" matched `aac`, so every Asimov EPUB read as
    // an audiobook and was grabbed. The two-word "audio book" is the one
    // signal that needs the substring form.
    if lower.contains("audio book") || tokens.iter().any(|t| AUDIO_SIGNALS.contains(t)) {
        return false; // has an audiobook signal — keep it
    }
    if size_bytes > 0 && size_bytes < MIN_AUDIOBOOK_BYTES {
        return true; // too small for a full audiobook
    }
    // Whole-token match so `pdf` in `pdfreader` (unlikely) or substrings don't fire.
    tokens.iter().any(|tok| EBOOK_FORMATS.contains(tok))
}

/// The abridgement of a parsed release, defaulting to `Unabridged` when the
/// release carries no explicit signal (the common case — most audiobooks are
/// unabridged and don't say so).
#[must_use]
pub fn abridgement_of(parsed: &ParsedRelease) -> Abridgement {
    match parsed.abridged {
        Some(true) => Abridgement::Abridged,
        _ => Abridgement::Unabridged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_token_mapping() {
        assert_eq!(AudioFormat::from_token("M4B"), Some(AudioFormat::M4b));
        assert_eq!(AudioFormat::from_token(".mp3"), Some(AudioFormat::Mp3));
        assert_eq!(AudioFormat::from_token("FLAC"), Some(AudioFormat::Flac));
        assert_eq!(AudioFormat::from_token("mkv"), None);
    }

    #[test]
    fn reconcile_quality_prefers_the_actual_file_format() {
        let defs = default_audiobook_definitions();
        let id = |name: &str| defs.iter().find(|d| d.name == name).unwrap().id;

        // The live bug: release parsed as M4B-256 but the files are MP3 → drop to
        // the highest MP3 tier at/below 256 kbps (MP3-128).
        assert_eq!(
            reconcile_quality_with_format(Some(id("M4B-256")), AudioFormat::Mp3, &defs),
            Some(id("MP3-128"))
        );
        // Parsed M4B-64 + MP3 files → MP3-64 (same bitrate exists).
        assert_eq!(
            reconcile_quality_with_format(Some(id("M4B-64")), AudioFormat::Mp3, &defs),
            Some(id("MP3-64"))
        );
        // Formats already agree → unchanged.
        assert_eq!(
            reconcile_quality_with_format(Some(id("MP3-128")), AudioFormat::Mp3, &defs),
            Some(id("MP3-128"))
        );
        // An MP3 parse but M4B files → up to the M4B tier at/below the bitrate.
        assert_eq!(
            reconcile_quality_with_format(Some(id("MP3-64")), AudioFormat::M4b, &defs),
            Some(id("M4B-64"))
        );
        // No parsed quality + MP3 files → the lowest MP3 tier.
        assert_eq!(
            reconcile_quality_with_format(None, AudioFormat::Mp3, &defs),
            Some(id("MP3-32"))
        );
        // Unmodeled file format (FLAC) → keep what we had.
        assert_eq!(
            reconcile_quality_with_format(Some(id("M4B-256")), AudioFormat::Flac, &defs),
            Some(id("M4B-256"))
        );
    }

    #[test]
    fn ranks_every_m4b_above_every_mp3() {
        // Format-first policy: the list order is the rank, and all M4B tiers must
        // sit above all MP3 tiers (so any M4B is preferred over any MP3).
        let defs = default_audiobook_definitions();
        let last_mp3 = defs
            .iter()
            .rposition(|d| d.format == AudioFormat::Mp3)
            .expect("has mp3 tiers");
        let first_m4b = defs
            .iter()
            .position(|d| d.format == AudioFormat::M4b)
            .expect("has m4b tiers");
        assert!(
            last_mp3 < first_m4b,
            "every MP3 must rank below every M4B (last MP3 at {last_mp3}, first M4B at {first_m4b})"
        );
        // Concretely: M4B-64 (lowest M4B) outranks MP3-128 (highest MP3).
        let rank = |name: &str| defs.iter().position(|d| d.name == name).unwrap();
        assert!(rank("M4B-64") > rank("MP3-128"));
        // The cutoff (highest) is still M4B-256.
        assert_eq!(defs.last().unwrap().name, "M4B-256");
    }

    #[test]
    fn bitrate_buckets_and_orders_low_to_high() {
        assert_eq!(AudioBitrate::from_kbps(64), AudioBitrate::Kbps64);
        assert_eq!(AudioBitrate::from_kbps(130), AudioBitrate::Kbps128);
        assert_eq!(AudioBitrate::from_kbps(320), AudioBitrate::Kbps256);
        assert_eq!(
            AudioBitrate::from_token("128kbps"),
            Some(AudioBitrate::Kbps128)
        );
        assert_eq!(AudioBitrate::from_token("vbr"), Some(AudioBitrate::Vbr));
        assert_eq!(AudioBitrate::from_token("nope"), None);
        // Ordering: VBR lowest, then ascending tiers.
        assert!(AudioBitrate::Vbr < AudioBitrate::Kbps32);
        assert!(AudioBitrate::Kbps64 < AudioBitrate::Kbps256);
    }

    #[test]
    fn abridgement_token_and_default() {
        assert_eq!(
            Abridgement::from_token("Abridged"),
            Some(Abridgement::Abridged)
        );
        assert_eq!(
            Abridgement::from_token("unabridged"),
            Some(Abridgement::Unabridged)
        );
        // No explicit signal → defaults to Unabridged.
        let p = ParsedRelease::default();
        assert_eq!(abridgement_of(&p), Abridgement::Unabridged);
        let p = ParsedRelease {
            abridged: Some(true),
            ..Default::default()
        };
        assert_eq!(abridgement_of(&p), Abridgement::Abridged);
    }

    #[test]
    fn definitions_are_stable_and_ranked_with_unique_ids() {
        let a = default_audiobook_definitions();
        let b = default_audiobook_definitions();
        assert_eq!(a, b, "definition ids are deterministic");
        assert!(a.len() >= 5);
        // Ids unique and disjoint from the movie definition id space (1..12).
        let movie_ids: Vec<_> = crate::quality::default_definitions()
            .into_iter()
            .map(|d| d.id)
            .collect();
        for d in &a {
            assert!(
                !movie_ids.contains(&d.id),
                "audiobook id collides with movie id"
            );
        }
    }

    #[test]
    fn maps_parsed_release_to_definition() {
        let defs = default_audiobook_definitions();
        let parsed = ParsedRelease {
            audio_format: Some("m4b".into()),
            bitrate_kbps: Some(128),
            ..Default::default()
        };
        let id = to_audiobook_quality(&parsed, &defs).expect("maps to M4B-128");
        let def = defs.iter().find(|d| d.id == id).unwrap();
        assert_eq!(def.name, "M4B-128");
        assert_ne!(
            id,
            unknown_audiobook_id(),
            "a tagged release is never Unknown"
        );

        // No format / no bitrate (e.g. a "(Audiobook)"-only title) → Unknown, so
        // it's still acquirable rather than silently dropped (SKADI-T-0175).
        let bare = ParsedRelease::default();
        assert_eq!(
            to_audiobook_quality(&bare, &defs),
            Some(unknown_audiobook_id())
        );

        // Format present but no modeled tier (FLAC isn't in the ladder) → Unknown.
        let flac = ParsedRelease {
            audio_format: Some("flac".into()),
            bitrate_kbps: Some(128),
            ..Default::default()
        };
        assert_eq!(
            to_audiobook_quality(&flac, &defs),
            Some(unknown_audiobook_id())
        );
    }

    #[test]
    fn unknown_tier_is_lowest_rank_and_in_the_default_profile() {
        let defs = default_audiobook_definitions();
        // Unknown is the first (lowest-rank) row.
        assert_eq!(defs[0].id, unknown_audiobook_id());
        assert_eq!(defs[0].name, "Unknown");
        // It outranks nothing: every real tier sits above it.
        assert!(defs.len() >= 8);
    }

    #[test]
    fn ebook_discriminator() {
        let big = 400 * 1024 * 1024; // 400 MiB — audiobook-sized
        let tiny = 3 * 1024 * 1024; // 3 MiB — ebook-sized
        // Audio signal wins regardless of size or an ebook token (combined packs).
        assert!(!looks_like_ebook(
            "Project Hail Mary (Audiobook)(Fiction)",
            0
        ));
        assert!(!looks_like_ebook(
            "Mistborn unabridged M4B + EPUB bonus",
            tiny
        ));
        assert!(!looks_like_ebook("Some Book [MP3, 128 kbps]", big));
        // Explicit ebook format, no audio signal → ebook.
        assert!(looks_like_ebook("Project Hail Mary - Andy Weir EPUB", big));
        assert!(looks_like_ebook("Dune retail (azw3)", big));
        // Too small + no audio signal → ebook/sample.
        assert!(looks_like_ebook(
            "Dungeon Crawler Carl - Matt Dinniman",
            tiny
        ));
        // Format-less, reasonably sized, no ebook token → treat as audiobook.
        assert!(!looks_like_ebook(
            "Dungeon Crawler Carl (Dungeon Crawler Carl 01) by Matt Dinniman",
            big
        ));
        // Substring safety: "pdf" buried in a word does NOT fire (no audio signal,
        // sized OK) — only a whole-token ebook format does.
        assert!(!looks_like_ebook("The Pdfeiffer Chronicles - Author", big));
    }

    #[test]
    fn reconcile_leaves_unknown_untouched() {
        let defs = default_audiobook_definitions();
        // An Unknown-acquired release keeps Unknown regardless of the file format
        // (we don't read the file's bitrate, so we don't fabricate a tier).
        assert_eq!(
            reconcile_quality_with_format(Some(unknown_audiobook_id()), AudioFormat::Mp3, &defs),
            Some(unknown_audiobook_id())
        );
        assert_eq!(
            reconcile_quality_with_format(Some(unknown_audiobook_id()), AudioFormat::M4b, &defs),
            Some(unknown_audiobook_id())
        );
    }
    /// "Isaac" contains `aac`: the substring form of the audio-signal check
    /// waved every Asimov EPUB through as an audiobook (grabbed on prod
    /// 2026-09-18). Signals are whole tokens now.
    #[test]
    fn ebook_check_matches_audio_signals_as_whole_tokens() {
        assert!(looks_like_ebook(
            "Isaac Asimov Lucky Starr and the Big Sun of Mercury 1956 epub",
            0
        ));
        assert!(looks_like_ebook("Isaac Asimov - Foundation (1951).epub", 0));
        assert!(!looks_like_ebook("Isaac Asimov - Foundation [M4B]", 0));
        assert!(!looks_like_ebook("Foundation - Isaac Asimov AAC 64kbps", 0));
        assert!(!looks_like_ebook(
            "Foundation - Isaac Asimov (Audio Book)",
            0
        ));
    }
}
