//! The quality taxonomy: the structured vocabulary of resolution, source,
//! codec, and modifier, plus the [`Quality`] tuple that combines them.
//!
//! Re-derived clean-room from observable release conventions. The `from_token`
//! helpers map the free-form tokens a parser pulls from a title onto these
//! typed variants; the parser (SKADI-T-0013) uses them to turn a
//! [`ParsedRelease`](crate::ParsedRelease) into a [`Quality`].

use serde::{Deserialize, Serialize};
use skadi_core::QualityId;
use uuid::Uuid;

/// Display resolution, ordered low → high (the `Ord` derive follows declaration
/// order, so `Resolution::Sd < Resolution::R2160p`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Resolution {
    Sd,
    R480p,
    R576p,
    R720p,
    R1080p,
    R2160p,
}

impl Resolution {
    /// Map a title token (case-insensitive) to a resolution.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "2160p" | "4k" | "uhd" => Some(Self::R2160p),
            "1080p" | "1080i" => Some(Self::R1080p),
            "720p" => Some(Self::R720p),
            "576p" => Some(Self::R576p),
            "480p" => Some(Self::R480p),
            "sd" => Some(Self::Sd),
            _ => None,
        }
    }
}

/// Where the release was sourced from.
///
/// `Ord` follows the variant order, which is the quality ladder from worst to
/// best — a cam is worse than a telesync is worse than a DVD, and so on. That
/// ordering is relied on by `quality_from_probe` (SKADI-T-0528) to pick the best
/// source defined at a resolution, so the variants must stay in ladder order.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Source {
    Cam,
    Telesync,
    Dvd,
    Hdtv,
    WebRip,
    WebDl,
    Bluray,
}

impl Source {
    /// Map a title token (case-insensitive) to a source.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token
            .to_ascii_lowercase()
            .replace(['-', '.', '_'], "")
            .as_str()
        {
            "bluray" | "bdrip" | "brrip" | "bdremux" | "brdisk" | "bddisk" => Some(Self::Bluray),
            // Bare `WEB` is a WEB-DL: that is what Sonarr calls it, and the
            // alternative (no source at all) meant the release never classified
            // (SKADI-T-0432).
            "webdl" | "web" => Some(Self::WebDl),
            "webrip" => Some(Self::WebRip),
            "hdtv" => Some(Self::Hdtv),
            "dvd" | "dvdrip" => Some(Self::Dvd),
            "ts" | "telesync" => Some(Self::Telesync),
            "cam" => Some(Self::Cam),
            _ => None,
        }
    }
}

/// Video codec.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    X264,
    X265,
    Av1,
}

impl Codec {
    /// Map a title token (case-insensitive) to a codec. `HEVC`/`H.265` fold into
    /// `X265` and `AVC`/`H.264` into `X264` — they name the same codec families.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token
            .to_ascii_lowercase()
            .replace(['.', '-', ' '], "")
            .as_str()
        {
            "x265" | "h265" | "hevc" => Some(Self::X265),
            "x264" | "h264" | "avc" => Some(Self::X264),
            "av1" => Some(Self::Av1),
            _ => None,
        }
    }
}

/// A quality-affecting modifier on a release.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Modifier {
    Remux,
    Proper,
    Repack,
}

impl Modifier {
    /// Map a title token (case-insensitive) to a modifier.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "remux" => Some(Self::Remux),
            "proper" => Some(Self::Proper),
            "repack" => Some(Self::Repack),
            _ => None,
        }
    }
}

/// The structured quality of a release: resolution + source, with optional
/// codec and modifier. `id` references a [`QualityDefinition`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quality {
    pub id: QualityId,
    pub resolution: Resolution,
    pub source: Source,
    pub codec: Option<Codec>,
    pub modifier: Option<Modifier>,
}

/// A named, stable quality definition — the unit a [`QualityProfile`] orders and
/// selects from. IDs are deterministic so defaults are stable across instances.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityDefinition {
    pub id: QualityId,
    pub name: String,
    pub resolution: Resolution,
    pub source: Source,
}

/// Deterministic `QualityId` for built-in definitions (stable across runs).
fn def_id(n: u128) -> QualityId {
    QualityId(Uuid::from_u128(n))
}

/// The quality of a file we have **not been able to assess** — an adopted library
/// file whose name carries no quality tokens, or an import whose title and probe
/// together yield no definition (SKADI-T-0399 / SKADI-T-0412).
///
/// Deliberately NOT a member of [`default_definitions`] and never part of a
/// profile ladder: it is not a tier, it is the absence of one. Consumers must
/// treat it as "do not judge" rather than "lowest" — the upgrade sweeps skip it
/// instead of seeing rank 0 and re-grabbing the whole library, which is exactly
/// what the SDTV floor used to cause.
pub const UNKNOWN_QUALITY_ID: QualityId = QualityId(Uuid::from_u128(0));

/// `true` when `id` is the unassessed-quality marker ([`UNKNOWN_QUALITY_ID`]).
#[must_use]
pub fn is_unknown_quality(id: QualityId) -> bool {
    id == UNKNOWN_QUALITY_ID
}

/// The definition row describing [`UNKNOWN_QUALITY_ID`], for display only
/// (`"Unknown"`); it is never returned by [`default_definitions`].
#[must_use]
pub fn unknown_definition() -> QualityDefinition {
    QualityDefinition {
        id: UNKNOWN_QUALITY_ID,
        name: "Unknown".to_string(),
        resolution: Resolution::Sd,
        source: Source::Hdtv,
    }
}

/// The baseline ranked list of quality definitions (low → high), re-derived
/// clean-room. Profiles select an ordered subset of these.
#[must_use]
pub fn default_definitions() -> Vec<QualityDefinition> {
    use Resolution::*;
    use Source::*;
    let rows: &[(u128, &str, Resolution, Source)] = &[
        (1, "SDTV", Sd, Hdtv),
        (2, "DVD", Sd, Dvd),
        (3, "HDTV-720p", R720p, Hdtv),
        (4, "WEBRip-720p", R720p, WebRip),
        (5, "WEBDL-720p", R720p, WebDl),
        (6, "Bluray-720p", R720p, Bluray),
        (7, "HDTV-1080p", R1080p, Hdtv),
        (8, "WEBRip-1080p", R1080p, WebRip),
        (9, "WEBDL-1080p", R1080p, WebDl),
        (10, "Bluray-1080p", R1080p, Bluray),
        (11, "WEBDL-2160p", R2160p, WebDl),
        (12, "Bluray-2160p", R2160p, Bluray),
    ];
    rows.iter()
        .map(|(n, name, resolution, source)| QualityDefinition {
            id: def_id(*n),
            name: (*name).to_string(),
            resolution: *resolution,
            source: *source,
        })
        .collect()
}

/// The Radarr/Sonarr-style size ceiling for one quality, in **MB per minute of
/// runtime** (SKADI-T-0439).
///
/// The numbers follow Radarr's shipped quality definitions. They are a sanity
/// ceiling, not a preference: a release under the cap is not thereby better, it
/// is merely plausible. Only the *max* is modelled — min/preferred belong to a
/// scoring axis we do not have, and a min would reject legitimately small encodes.
///
/// Remux is **not** handled here. Remux is a [`Modifier`], not a [`Source`], so a
/// remux release classifies to the same Bluray definition as an encode while
/// being several times its size. Callers that can see the release title must skip
/// this ceiling for a remux; see `pipeline::quality_size_ok`.
#[must_use]
pub fn max_mb_per_minute(def: &QualityDefinition) -> f64 {
    match def.resolution {
        Resolution::R2160p => 350.0,
        Resolution::R1080p => 227.0,
        Resolution::R720p => 137.0,
        // Everything SD-or-unknown shares Radarr's SDTV/DVD ceiling.
        _ => 100.0,
    }
}

/// The absolute size ceiling for one quality over `runtime_minutes`, in bytes.
///
/// We do not track per-item runtime, so callers pass a nominal figure; see
/// `pipeline::quality_size_ok` for how that assumption is bounded.
#[must_use]
pub fn max_size_bytes(def: &QualityDefinition, runtime_minutes: u32) -> u64 {
    (max_mb_per_minute(def) * f64::from(runtime_minutes) * 1024.0 * 1024.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_orders_low_to_high() {
        assert!(Resolution::Sd < Resolution::R720p);
        assert!(Resolution::R1080p < Resolution::R2160p);
        let mut v = vec![Resolution::R2160p, Resolution::Sd, Resolution::R1080p];
        v.sort();
        assert_eq!(
            v,
            vec![Resolution::Sd, Resolution::R1080p, Resolution::R2160p]
        );
    }

    #[test]
    fn token_mapping() {
        assert_eq!(Resolution::from_token("1080p"), Some(Resolution::R1080p));
        assert_eq!(Resolution::from_token("UHD"), Some(Resolution::R2160p));
        assert_eq!(Source::from_token("WEB-DL"), Some(Source::WebDl));
        assert_eq!(Source::from_token("BluRay"), Some(Source::Bluray));
        assert_eq!(Codec::from_token("HEVC"), Some(Codec::X265));
        assert_eq!(Codec::from_token("H.264"), Some(Codec::X264));
        assert_eq!(Modifier::from_token("Repack"), Some(Modifier::Repack));
        assert_eq!(Resolution::from_token("nonsense"), None);
    }

    #[test]
    fn default_definitions_are_stable_and_ranked() {
        let a = default_definitions();
        let b = default_definitions();
        assert_eq!(a, b, "definition ids are deterministic");
        assert!(a.len() >= 10);
        // Resolution is non-decreasing down the ranked list.
        assert!(a.windows(2).all(|w| w[0].resolution <= w[1].resolution));
    }

    #[test]
    fn quality_round_trips() {
        let q = Quality {
            id: def_id(10),
            resolution: Resolution::R1080p,
            source: Source::Bluray,
            codec: Some(Codec::X264),
            modifier: None,
        };
        let json = serde_json::to_string(&q).unwrap();
        let back: Quality = serde_json::from_str(&json).unwrap();
        assert_eq!(q, back);
    }
}
