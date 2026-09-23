//! `MovieStatusSink` — `StatusSink` impl for the movies domain (SKADI-T-0048).
//!
//! Decodes the `AcquirableRef` carried by the hunter back to a
//! `MovieEditionId` and persists the [`AcquisitionStatus`] via `MoviesRepo`.
//! Closes the v0 deferral from T-0030 (which shipped only `InMemoryStatusSink`).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use skadi_core::{AcquisitionStatus, AppError, MovieEditionId, Result};
use skadi_hunter::StatusSink;
use skadi_importer::AcquirableRef;
use skadi_quality::default_definitions;
use skadi_store::{HistoryEntry, HistoryRepo};

use crate::repo::MoviesRepo;

pub struct MovieStatusSink {
    repo: Arc<dyn MoviesRepo>,
    history: Arc<dyn HistoryRepo>,
}

impl MovieStatusSink {
    #[must_use]
    pub fn new(repo: Arc<dyn MoviesRepo>, history: Arc<dyn HistoryRepo>) -> Self {
        Self { repo, history }
    }

    /// Resolve a movie title for the history label, best-effort.
    async fn title_for(&self, id: MovieEditionId) -> Option<String> {
        let edition = self.repo.get_edition(id).await.ok().flatten()?;
        let movie = self.repo.get_movie(edition.movie_id).await.ok().flatten()?;
        Some(movie.title)
    }
}

/// Map an acquisition status to a history event, or `None` for the transient
/// states we don't log (Missing/Searching/Downloading/Cutoff). Returns
/// `(event, detail, reason_code, at)` — `reason_code` is the structured failure
/// code for `failed` events (SKADI-T-0200), `None` otherwise.
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
impl StatusSink for MovieStatusSink {
    async fn set_status(
        &self,
        acquirable: &AcquirableRef,
        status: AcquisitionStatus,
    ) -> Result<()> {
        let id = decode(acquirable)?;
        // Compute the history event before `status` is moved into the repo.
        let event = history_event(&status);
        // Repo's `set_edition_status` already does the right thing for both
        // backends: writes status_json + status_kind + updated_at, plus
        // file/quality/format_score on `Imported`. We don't need a second
        // round-trip here.
        self.repo.set_edition_status(id, status).await?;

        // Append to the persistent acquisition history (best-effort — never fail
        // the acquire workflow because history recording failed). SKADI-T-0082.
        if let Some((event, detail, reason_code, at)) = event {
            let label = self
                .title_for(id)
                .await
                .unwrap_or_else(|| "(unknown)".into());
            let entry = HistoryEntry {
                id: Uuid::new_v4().to_string(),
                at,
                kind: "movie".into(),
                acquirable_ref: acquirable.0.clone(),
                label,
                event: event.into(),
                detail,
                reason_code,
            };
            if let Err(e) = self.history.record_history(&entry).await {
                tracing::warn!(error = %e, "failed to record acquisition history");
            }
        }
        Ok(())
    }

    async fn get_status(&self, acquirable: &AcquirableRef) -> Result<Option<AcquisitionStatus>> {
        let id = decode(acquirable)?;
        Ok(self.repo.get_edition(id).await?.map(|e| e.status))
    }

    async fn set_media_info(
        &self,
        acquirable: &AcquirableRef,
        media_info: skadi_core::MediaInfo,
    ) -> Result<()> {
        let id = decode(acquirable)?;
        if let Some(mut edition) = self.repo.get_edition(id).await? {
            edition.media_info = Some(media_info);
            self.repo.upsert_edition(&edition).await?;
        }
        Ok(())
    }
}

fn decode(r: &AcquirableRef) -> Result<MovieEditionId> {
    Uuid::parse_str(&r.0)
        .map(MovieEditionId::from)
        .map_err(|e| AppError::Validation(format!("invalid movie AcquirableRef: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use diesel::connection::Connection;
    use diesel::sqlite::SqliteConnection;
    use diesel_migrations::MigrationHarness;
    use skadi_core::{
        AcquisitionStatus, EditionKindId, ExternalIds, FailureReason, FileRef, ProfileId,
        QualityId, RootFolder, TmdbId,
    };
    use skadi_store::{HistoryRepo, Store};

    use crate::edition::MovieEdition;
    use crate::movie::Movie;
    use crate::repo::MoviesRepo;
    use crate::{SQLITE_MIGRATIONS, THEATRICAL_KIND_ID};

    async fn fresh_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skadi.db");
        let url = format!("sqlite://{}", path.display());
        let s = Store::connect(&url).unwrap();
        s.run_migrations().await.unwrap();
        drop(s);
        let mut conn = SqliteConnection::establish(&path.display().to_string()).unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
        (dir, Store::connect(&url).unwrap())
    }

    async fn seed_movie_with_edition(store: &Store) -> MovieEdition {
        let movie = Movie {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::MovieId::new(),
            external_ids: ExternalIds {
                tmdb: Some(TmdbId(603)),
                ..Default::default()
            },
            title: "The Matrix".into(),
            original_title: None,
            year: Some(1999),
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            collection: None,
            monitored: true,
            profile: ProfileId::new(),
            root_folder: RootFolder::new("/movies"),
            added_at: Utc::now(),
            last_metadata_refresh: None,
            editions: Vec::new(),
        };
        store.upsert_movie(&movie).await.unwrap();
        let edition = MovieEdition::missing(movie.id, EditionKindId::from(THEATRICAL_KIND_ID));
        store.upsert_edition(&edition).await.unwrap();
        edition
    }

    #[tokio::test]
    async fn set_then_get_round_trips_every_variant() {
        let (_d, store) = fresh_store().await;
        let edition = seed_movie_with_edition(&store).await;
        let s = Arc::new(store);
        let sink = MovieStatusSink::new(s.clone(), s);
        let aref = edition.acquirable_ref();

        for status in [
            AcquisitionStatus::Searching {
                since: Utc::now(),
                attempts: 1,
            },
            AcquisitionStatus::Cutoff,
            AcquisitionStatus::Failed {
                reason: FailureReason::NoSuitableRelease,
                retry_at: None,
                attempts: 0,
            },
        ] {
            sink.set_status(&aref, status.clone()).await.unwrap();
            let got = sink.get_status(&aref).await.unwrap().unwrap();
            assert_eq!(
                std::mem::discriminant(&got),
                std::mem::discriminant(&status),
                "variant round-trip for {status:?}"
            );
        }
    }

    #[tokio::test]
    async fn imported_status_writes_file_quality_and_score_columns() {
        let (_d, store) = fresh_store().await;
        let edition = seed_movie_with_edition(&store).await;
        let store_arc = Arc::new(store);
        let sink = MovieStatusSink::new(store_arc.clone(), store_arc.clone());

        let qid = QualityId::new();
        sink.set_status(
            &edition.acquirable_ref(),
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/movies/The Matrix (1999)/The Matrix (1999).mkv".into(),
                },
                quality: qid,
                score: 17,
                at: Utc::now(),
            },
        )
        .await
        .unwrap();

        let row = store_arc.get_edition(edition.id).await.unwrap().unwrap();
        assert!(matches!(row.status, AcquisitionStatus::Imported { .. }));
        assert!(row.file.is_some());
        assert_eq!(row.quality, Some(qid));
        assert_eq!(row.format_score, 17);
    }

    #[tokio::test]
    async fn set_media_info_persists_probed_streams_on_the_edition() {
        let (_d, store) = fresh_store().await;
        let edition = seed_movie_with_edition(&store).await;
        let store_arc = Arc::new(store);
        let sink = MovieStatusSink::new(store_arc.clone(), store_arc.clone());

        let info = skadi_core::MediaInfo {
            duration_secs: Some(8160),
            video: Some(skadi_core::VideoInfo {
                width: 1920,
                height: 1080,
                codec: Some("h264".into()),
                profile: None,
                dynamic_range: None,
            }),
            audio: Some(skadi_core::AudioInfo {
                codec: Some("aac".into()),
                channels: Some(2),
                bitrate_kbps: None,
                sample_rate_hz: Some(48000),
                ..Default::default()
            }),
            ..Default::default()
        };
        sink.set_media_info(&edition.acquirable_ref(), info.clone())
            .await
            .unwrap();

        let row = store_arc.get_edition(edition.id).await.unwrap().unwrap();
        assert_eq!(
            row.media_info.as_ref(),
            Some(&info),
            "round-trips via JSON column"
        );
        assert_eq!(
            row.media_info.unwrap().video.unwrap().resolution_tier(),
            "1080p"
        );
    }

    #[tokio::test]
    async fn idempotent_overwrite_is_a_noop_at_the_data_level() {
        let (_d, store) = fresh_store().await;
        let edition = seed_movie_with_edition(&store).await;
        let s = Arc::new(store);
        let sink = MovieStatusSink::new(s.clone(), s);
        let aref = edition.acquirable_ref();
        let status = AcquisitionStatus::Cutoff;
        sink.set_status(&aref, status.clone()).await.unwrap();
        sink.set_status(&aref, status.clone()).await.unwrap();
        let again = sink.get_status(&aref).await.unwrap().unwrap();
        assert!(matches!(again, AcquisitionStatus::Cutoff));
    }

    #[tokio::test]
    async fn records_history_on_meaningful_transitions_only() {
        let (_d, store) = fresh_store().await;
        let edition = seed_movie_with_edition(&store).await;
        let s = Arc::new(store);
        let sink = MovieStatusSink::new(s.clone(), s.clone());
        let aref = edition.acquirable_ref();

        // Searching is transient — recorded as nothing.
        sink.set_status(
            &aref,
            AcquisitionStatus::Searching {
                since: Utc::now(),
                attempts: 1,
            },
        )
        .await
        .unwrap();
        assert!(s.list_history(10, 0).await.unwrap().is_empty());

        // Imported records one row, labelled with the movie title.
        sink.set_status(
            &aref,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/movies/x.mkv".into(),
                },
                quality: QualityId::new(),
                score: 0,
                at: Utc::now(),
            },
        )
        .await
        .unwrap();
        let hist = s.list_history(10, 0).await.unwrap();
        assert_eq!(hist.len(), 1, "only the Imported transition recorded");
        assert_eq!(hist[0].event, "imported");
        assert_eq!(hist[0].kind, "movie");
        assert!(
            !hist[0].label.is_empty() && hist[0].label != "(unknown)",
            "label resolved to the movie title, got {:?}",
            hist[0].label
        );
    }

    #[tokio::test]
    async fn malformed_acquirable_ref_returns_validation_error() {
        let (_d, store) = fresh_store().await;
        let s = Arc::new(store);
        let sink = MovieStatusSink::new(s.clone(), s);
        let bad = AcquirableRef("not-a-uuid".into());
        let err = sink
            .set_status(&bad, AcquisitionStatus::Cutoff)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
        let err2 = sink.get_status(&bad).await.unwrap_err();
        assert!(matches!(err2, AppError::Validation(_)));
    }
}
