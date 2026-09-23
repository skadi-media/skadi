//! The two shapes of a TV acquirable reference (SKADI-T-0590).
//!
//! The hunter identifies what a run acquires by an opaque [`AcquirableRef`].
//! TV hands it two kinds: a single episode (the episode's UUID) and a **season
//! pack** (`season-<series uuid>-<season>`, emitted by the wanted query once
//! enough of a season is missing). Every consumer that decoded the ref assumed
//! the first shape, so a pack downloaded in full and then failed at the first
//! line of its import with "bad acquirable ref: invalid character: found `s`
//! at 1" — the MST3K season 13 pack, 48 GB, on 2026-09-17. This is the one
//! place both shapes are built and parsed.

use skadi_core::{AppError, EpisodeId, Result, SeriesId};
use skadi_importer::AcquirableRef;
use uuid::Uuid;

/// Prefix of a season-pack ref.
const SEASON_PREFIX: &str = "season-";

/// What a TV [`AcquirableRef`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TvAcquirable {
    /// One episode.
    Episode(EpisodeId),
    /// A whole season of one series, acquired as a pack. The importer's
    /// per-file matcher places each episode it contains.
    Season { series: SeriesId, season: u16 },
}

impl TvAcquirable {
    /// The ref for a season-pack run: `season-<series>-<season>`.
    #[must_use]
    pub fn season_ref(series: SeriesId, season: u16) -> AcquirableRef {
        AcquirableRef(format!("{SEASON_PREFIX}{series}-{season}"))
    }

    /// Decode either shape.
    ///
    /// # Errors
    /// [`AppError::Validation`] when the ref is neither an episode UUID nor a
    /// well-formed season ref.
    pub fn parse(r: &AcquirableRef) -> Result<Self> {
        if let Some(rest) = r.0.strip_prefix(SEASON_PREFIX) {
            // The series UUID carries its own dashes, so split at the last one.
            let (series, season) = rest.rsplit_once('-').ok_or_else(|| {
                AppError::Validation(format!("bad season acquirable ref: {}", r.0))
            })?;
            let series = Uuid::parse_str(series).map(SeriesId::from).map_err(|e| {
                AppError::Validation(format!("bad season acquirable ref {}: {e}", r.0))
            })?;
            let season: u16 = season.parse().map_err(|e| {
                AppError::Validation(format!("bad season acquirable ref {}: {e}", r.0))
            })?;
            return Ok(Self::Season { series, season });
        }
        Uuid::parse_str(&r.0)
            .map(EpisodeId::from)
            .map(Self::Episode)
            .map_err(|e| AppError::Validation(format!("bad acquirable ref: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn season_ref_round_trips() {
        let series = SeriesId::new();
        let r = TvAcquirable::season_ref(series, 13);
        assert!(r.0.starts_with("season-"));
        assert_eq!(
            TvAcquirable::parse(&r).unwrap(),
            TvAcquirable::Season { series, season: 13 }
        );
    }

    #[test]
    fn episode_ref_is_the_uuid() {
        let id = EpisodeId::new();
        let r = AcquirableRef(id.to_string());
        assert_eq!(TvAcquirable::parse(&r).unwrap(), TvAcquirable::Episode(id));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(TvAcquirable::parse(&AcquirableRef("not-a-uuid".into())).is_err());
        assert!(TvAcquirable::parse(&AcquirableRef("season-nope-1".into())).is_err());
        assert!(
            TvAcquirable::parse(&AcquirableRef(format!("season-{}-x", SeriesId::new()))).is_err()
        );
    }
}
