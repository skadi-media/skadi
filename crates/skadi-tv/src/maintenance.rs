//! One-shot maintenance passes over the television catalog.
//!
//! [`backfill_unassessed_quality`] repairs rows written before SKADI-T-0399:
//! adoption recorded the ladder's first tier (SDTV) for any episode whose file
//! name yielded no quality — 18,449 episodes on the operator's library — so the
//! upgrade sweep treated the whole catalog as below cutoff. Import now records
//! `UNKNOWN_QUALITY_ID`, which the sweep skips; this pass re-derives each stored
//! row's quality from its file name and stores Unknown where nothing derives.

use skadi_core::Result;
use skadi_core::status::AcquisitionStatus;

use crate::repo::{SeriesFilter, TvRepo};

/// What a backfill run did (or, in a dry run, would do).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Imported episodes examined.
    pub scanned: usize,
    /// Rows whose quality was re-derived from the file name.
    pub graded: usize,
    /// Rows moved to `UNKNOWN_QUALITY_ID` (nothing derivable).
    pub unknown: usize,
    /// Rows left alone (a genuinely assessed quality).
    pub kept: usize,
}

/// Re-grade imported episodes that hold the legacy floor tier or Unknown.
///
/// With `apply == false` nothing is written.
/// How many files are probed at once (SKADI-T-0557).
///
/// The cost of a probe is IO latency, not CPU — a header read over NFS — so
/// serial walking of ~20k files is dominated by waiting. A small amount of
/// concurrency hides most of that; a large amount just queues on the mount and
/// makes an interrupted run harder to reason about.
const PROBE_CONCURRENCY: usize = 8;

/// Grade imported episodes that are still `Unknown` by **probing the file**
/// (SKADI-T-0557, extending SKADI-T-0528 to television).
///
/// Only rows already `Unknown` are touched: a row the name pass graded has real
/// evidence behind it, and overwriting that with a probe's assumed source would
/// be a downgrade in confidence.
///
/// **Resumable by construction.** Each episode is written as it is graded, and
/// the pass only ever selects rows that are still `Unknown`, so re-running after
/// an interruption skips everything already done. No checkpoint is needed, and
/// building one would be state to keep correct for no gain.
pub async fn grade_unknown_by_probe(
    repo: &dyn TvRepo,
    prober: &(dyn skadi_media_probe::MediaProber + Sync),
    definitions: &[skadi_quality::QualityDefinition],
    apply: bool,
) -> Result<BackfillReport> {
    use futures::stream::{self, StreamExt};

    let mut report = BackfillReport::default();
    for series in repo.list_series(SeriesFilter::default()).await? {
        // Collect this series' candidates first so the probes can overlap.
        let candidates: Vec<_> = series
            .episodes
            .iter()
            .filter_map(|ep| match &ep.status {
                AcquisitionStatus::Imported {
                    file,
                    quality,
                    score,
                    at,
                } if skadi_quality::is_unknown_quality(*quality) => {
                    Some((ep.id, file.clone(), *score, *at))
                }
                AcquisitionStatus::Imported { .. } => None,
                _ => None,
            })
            .collect();
        report.kept += series
            .episodes
            .iter()
            .filter(|ep| {
                matches!(&ep.status, AcquisitionStatus::Imported { quality, .. }
                    if !skadi_quality::is_unknown_quality(*quality))
            })
            .count();

        // Probe with bounded concurrency; the writes stay sequential below so a
        // partial run leaves a coherent database rather than an interleaving.
        let probed: Vec<_> = stream::iter(candidates)
            .map(|(id, file, score, at)| async move {
                let graded = prober
                    .probe(&file.path)
                    .and_then(|info| skadi_quality::quality_from_probe(&info, definitions));
                (id, file, score, at, graded)
            })
            .buffer_unordered(PROBE_CONCURRENCY)
            .collect()
            .await;

        for (id, file, score, at, graded) in probed {
            report.scanned += 1;
            // A file that cannot be probed — missing, unreadable, a format the
            // prober does not know — stays Unknown. Never a guessed tier
            // (SKADI-T-0412).
            let Some(q) = graded else {
                report.unknown += 1;
                continue;
            };
            report.graded += 1;
            if apply {
                repo.set_episode_status(
                    id,
                    AcquisitionStatus::Imported {
                        file,
                        quality: q.id,
                        score,
                        at,
                    },
                )
                .await?;
            }
        }
    }
    Ok(report)
}

pub async fn backfill_unassessed_quality(repo: &dyn TvRepo, apply: bool) -> Result<BackfillReport> {
    let mut report = BackfillReport::default();
    for series in repo.list_series(SeriesFilter::default()).await? {
        for episode in &series.episodes {
            let AcquisitionStatus::Imported {
                file,
                quality,
                score,
                at,
            } = &episode.status
            else {
                continue;
            };
            report.scanned += 1;
            let name = file
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let Some(next) = skadi_quality::regraded_id(*quality, name, None) else {
                report.kept += 1;
                continue;
            };
            if skadi_quality::is_unknown_quality(next) {
                report.unknown += 1;
            } else {
                report.graded += 1;
            }
            if apply {
                repo.set_episode_status(
                    episode.id,
                    AcquisitionStatus::Imported {
                        file: file.clone(),
                        quality: next,
                        score: *score,
                        at: *at,
                    },
                )
                .await?;
            }
        }
    }
    Ok(report)
}
