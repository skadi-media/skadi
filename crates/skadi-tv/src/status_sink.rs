//! `EpisodeStatusSink` — `StatusSink` impl for the television domain
//! (SKADI-T-0270). Closes the acquire loop: decode the hunter's opaque
//! `AcquirableRef` back to an [`EpisodeId`], persist the new status, and append a
//! best-effort acquisition-history row. Mirrors `MovieStatusSink`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use skadi_core::{AcquisitionStatus, AppError, EpisodeId, Result};
use skadi_hunter::StatusSink;
use skadi_importer::AcquirableRef;
use skadi_quality::default_definitions;
use skadi_store::{HistoryEntry, HistoryRepo};

use crate::repo::TvRepo;

pub struct EpisodeStatusSink {
    repo: Arc<dyn TvRepo>,
    history: Arc<dyn HistoryRepo>,
}

impl EpisodeStatusSink {
    #[must_use]
    pub fn new(repo: Arc<dyn TvRepo>, history: Arc<dyn HistoryRepo>) -> Self {
        Self { repo, history }
    }

    /// `Series Title - SxxEyy` for the history label, best-effort.
    async fn label_for(&self, id: EpisodeId) -> Option<String> {
        let ep = self.repo.get_episode(id).await.ok().flatten()?;
        let series = self.repo.get_series(ep.series_id).await.ok().flatten()?;
        Some(format!(
            "{} - S{:02}E{:02}",
            series.title, ep.season, ep.number
        ))
    }
}

/// The episode a ref names, or `None` for a season-pack ref (SKADI-T-0590).
///
/// A pack run has no row of its own: its episodes are marked one by one by the
/// import fan-out (each placed file carries its episode's ref), so run-level
/// status for the pack is a no-op here rather than an error — which every
/// caller used to swallow, and the import step did not.
fn decode(r: &AcquirableRef) -> Result<Option<EpisodeId>> {
    match crate::acquirable::TvAcquirable::parse(r)
        .map_err(|e| AppError::Validation(format!("invalid episode AcquirableRef: {e}")))?
    {
        crate::acquirable::TvAcquirable::Episode(id) => Ok(Some(id)),
        crate::acquirable::TvAcquirable::Season { .. } => Ok(None),
    }
}

/// Map a status to a loggable history event, or `None` for transient states
/// (Missing/Searching/Downloading/Cutoff). Mirrors the movies sink.
fn history_event(
    status: &AcquisitionStatus,
) -> Option<(&'static str, Option<String>, Option<String>, DateTime<Utc>)> {
    match status {
        AcquisitionStatus::Snatched { at, .. } => Some(("grabbed", None, None, *at)),
        AcquisitionStatus::Imported { quality, at, .. } => {
            let name = default_definitions()
                .into_iter()
                .find(|d| d.id == *quality)
                .map(|d| d.name);
            Some(("imported", name, None, *at))
        }
        AcquisitionStatus::Failed { reason, .. } => Some((
            "failed",
            Some(format!("{reason:?}")),
            Some(reason.code().to_string()),
            Utc::now(),
        )),
        _ => None,
    }
}

#[async_trait]
impl StatusSink for EpisodeStatusSink {
    async fn set_status(
        &self,
        acquirable: &AcquirableRef,
        status: AcquisitionStatus,
    ) -> Result<()> {
        let Some(id) = decode(acquirable)? else {
            tracing::debug!(acquirable = %acquirable.0, ?status, "season-pack run status (no row)");
            return Ok(());
        };
        let event = history_event(&status);
        self.repo.set_episode_status(id, status).await?;

        if let Some((event, detail, reason_code, at)) = event {
            let label = self
                .label_for(id)
                .await
                .unwrap_or_else(|| "(unknown)".into());
            let entry = HistoryEntry {
                id: Uuid::new_v4().to_string(),
                at,
                kind: "episode".into(),
                acquirable_ref: acquirable.0.clone(),
                label,
                event: event.into(),
                detail,
                reason_code,
            };
            if let Err(e) = self.history.record_history(&entry).await {
                tracing::warn!(error = %e, "failed to record episode acquisition history");
            }
        }
        Ok(())
    }

    async fn get_status(&self, acquirable: &AcquirableRef) -> Result<Option<AcquisitionStatus>> {
        let Some(id) = decode(acquirable)? else {
            return Ok(None);
        };
        Ok(self.repo.get_episode(id).await?.map(|e| e.status))
    }

    /// Persist the post-import probe's result (SKADI-T-0451).
    ///
    /// TV used the trait's no-op default, so the probe ran, produced real
    /// resolution/codec/duration for the imported file, and the answer was
    /// discarded — movies and audiobooks both stored it. `episodes` had no column
    /// to store it in until this ticket added one.
    async fn set_media_info(
        &self,
        acquirable: &AcquirableRef,
        media_info: skadi_core::MediaInfo,
    ) -> Result<()> {
        let Some(id) = decode(acquirable)? else {
            return Ok(());
        };
        if let Some(mut episode) = self.repo.get_episode(id).await? {
            episode.media_info = Some(media_info);
            self.repo.upsert_episode(&episode).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_core::{ExternalIds, FileRef, ProfileId, QualityId, RootFolder, TvdbId};
    use skadi_store::Store;
    use skadi_testsupport::TestDb;
    use std::path::PathBuf;

    use crate::episode::Episode;
    use crate::series::Series;

    #[tokio::test]
    async fn set_status_persists_and_records_history() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store: Store = db.store.clone();

        let mut series = Series::new(
            ExternalIds {
                tvdb: Some(TvdbId(121361)),
                ..Default::default()
            },
            "Game of Thrones",
            ProfileId::new(),
            RootFolder::new("/tv"),
        );
        series.year = Some(2011);
        store.upsert_series(&series).await.unwrap();
        let episode = Episode::missing(series.id, 1, 5);
        store.upsert_episode(&episode).await.unwrap();

        let sink = EpisodeStatusSink::new(Arc::new(store.clone()), Arc::new(store.clone()));
        let r = episode.acquirable_ref();
        sink.set_status(
            &r,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: PathBuf::from("got.s01e05.mkv"),
                },
                quality: QualityId::new(),
                score: 50,
                at: Utc::now(),
            },
        )
        .await
        .unwrap();

        // Status persisted on the episode.
        let got = store.get_episode(episode.id).await.unwrap().unwrap();
        assert!(matches!(got.status, AcquisitionStatus::Imported { .. }));

        // History row written with the SxxEyy label.
        let hist = store.list_history(10, 0).await.unwrap();
        let row = hist.iter().find(|h| h.acquirable_ref == r.0).unwrap();
        assert_eq!(row.event, "imported");
        assert_eq!(row.kind, "episode");
        assert!(row.label.contains("S01E05"), "{}", row.label);
    }

    #[test]
    fn decode_rejects_garbage_ref() {
        assert!(decode(&AcquirableRef("not-a-uuid".into())).is_err());
    }

    /// A season-pack run's status writes are accepted and touch no row
    /// (SKADI-T-0590): the pack's episodes are marked by the import fan-out.
    #[tokio::test]
    async fn season_pack_ref_status_is_a_no_op() {
        let db = TestDb::new(crate::SQLITE_MIGRATIONS, crate::POSTGRES_MIGRATIONS).await;
        let store: Store = db.store.clone();
        let sink = EpisodeStatusSink::new(Arc::new(store.clone()), Arc::new(store.clone()));
        let r = crate::acquirable::TvAcquirable::season_ref(skadi_core::SeriesId::new(), 13);
        sink.set_status(
            &r,
            AcquisitionStatus::Searching {
                since: Utc::now(),
                attempts: 1,
            },
        )
        .await
        .expect("a season ref must not error");
        assert_eq!(sink.get_status(&r).await.unwrap(), None);
        assert!(store.list_history(10, 0).await.unwrap().is_empty());
    }
}
