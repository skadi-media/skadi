//! One-shot maintenance passes over the movies catalog.
//!
//! [`backfill_unassessed_quality`] repairs rows written before SKADI-T-0399:
//! adoption used to record the quality ladder's first tier (SDTV) for any file
//! whose name yielded no quality, so on a real library every such edition read
//! as "below cutoff" and enabling upgrades re-fetched the catalog. Import now
//! records `UNKNOWN_QUALITY_ID`, which the upgrade sweep skips; this pass brings
//! the existing rows to the same footing by re-deriving each file's quality from
//! its name and storing Unknown where nothing can be derived.

use skadi_core::Result;
use skadi_core::status::AcquisitionStatus;

use crate::repo::{MovieFilter, MoviesRepo};

/// What a backfill run did (or, in a dry run, would do).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Imported editions examined.
    pub scanned: usize,
    /// Rows whose quality was re-derived from the file name.
    pub graded: usize,
    /// Rows moved to `UNKNOWN_QUALITY_ID` (nothing derivable).
    pub unknown: usize,
    /// Rows left alone (a genuinely assessed quality).
    pub kept: usize,
}

/// Grade imported editions that are still `Unknown` by **probing the file**
/// (SKADI-T-0528).
///
/// The name-based pass above cannot help the adopted library: those rows carry
/// skadi's own canonical paths, which have no quality tokens, so all 19,803 of
/// them ended at `Unknown` — honest, but invisible to the upgrade sweep, which
/// deliberately skips Unknown.
///
/// Only rows that are already `Unknown` are touched. A row the name pass graded
/// has real evidence behind it; overwriting that with a probe would replace a
/// known source with an assumed one.
///
/// Cost is the reason this is a separate pass rather than a fallback inside the
/// name pass: probing reads each file's header, which over NFS for ~20k files is
/// minutes-to-hours, whereas the name pass is pure string work. An operator
/// should choose to pay it.
pub async fn grade_unknown_by_probe(
    repo: &dyn MoviesRepo,
    prober: &dyn skadi_media_probe::MediaProber,
    definitions: &[skadi_quality::QualityDefinition],
    apply: bool,
) -> Result<BackfillReport> {
    let mut report = BackfillReport::default();
    for movie in repo.list_movies(MovieFilter::default()).await? {
        for edition in &movie.editions {
            let AcquisitionStatus::Imported {
                file,
                quality,
                score,
                at,
            } = &edition.status
            else {
                continue;
            };
            // Only the rows nothing else could grade.
            if !skadi_quality::is_unknown_quality(*quality) {
                report.kept += 1;
                continue;
            }
            report.scanned += 1;
            // A file that cannot be probed — missing, unreadable, a format the
            // prober does not know — stays Unknown. That is the honest answer,
            // and it is what SKADI-T-0399 established: never invent a tier.
            let Some(info) = prober.probe(&file.path) else {
                report.unknown += 1;
                continue;
            };
            let Some(graded) = skadi_quality::quality_from_probe(&info, definitions) else {
                report.unknown += 1;
                continue;
            };
            report.graded += 1;
            if apply {
                repo.set_edition_status(
                    edition.id,
                    AcquisitionStatus::Imported {
                        file: file.clone(),
                        quality: graded.id,
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

/// Re-grade imported editions that hold the legacy floor tier or Unknown.
///
/// With `apply == false` nothing is written — the report describes what a real
/// run would change.
pub async fn backfill_unassessed_quality(
    repo: &dyn MoviesRepo,
    apply: bool,
) -> Result<BackfillReport> {
    let mut report = BackfillReport::default();
    for movie in repo.list_movies(MovieFilter::default()).await? {
        for edition in &movie.editions {
            let AcquisitionStatus::Imported {
                file,
                quality,
                score,
                at,
            } = &edition.status
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
                repo.set_edition_status(
                    edition.id,
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

#[cfg(test)]
pub(crate) mod tests {
    use diesel::Connection;
    use diesel::sqlite::SqliteConnection;
    use diesel_migrations::MigrationHarness;
    use skadi_core::FileRef;
    use skadi_store::Store;

    use super::*;
    use crate::SQLITE_MIGRATIONS;
    use crate::THEATRICAL_KIND_ID;
    use crate::edition::MovieEdition;
    use crate::movie::Movie;

    pub(super) async fn fresh_store() -> (tempfile::TempDir, Store) {
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

    /// Distinct tmdb ids so the three fixtures are separate rows.
    fn rand_tmdb() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(600);
        N.fetch_add(1, Ordering::Relaxed)
    }

    pub(super) async fn save(
        store: &Store,
        title: &str,
        file: &str,
        quality: skadi_core::QualityId,
    ) {
        let movie = Movie {
            content_rating: None,
            genres: Vec::new(),
            id: skadi_core::MovieId::new(),
            external_ids: skadi_core::ExternalIds {
                tmdb: Some(skadi_core::TmdbId(rand_tmdb())),
                ..Default::default()
            },
            title: title.to_string(),
            original_title: None,
            year: Some(1999),
            overview: None,
            runtime_minutes: None,
            poster_url: None,
            backdrop_url: None,
            collection: None,
            monitored: true,
            profile: skadi_core::ProfileId::new(),
            root_folder: skadi_core::RootFolder::new("/movies"),
            added_at: chrono::Utc::now(),
            last_metadata_refresh: None,
            editions: Vec::new(),
        };
        store.upsert_movie(&movie).await.unwrap();
        let mut ed = MovieEdition::missing(
            movie.id,
            skadi_core::EditionKindId::from(THEATRICAL_KIND_ID),
        );
        ed.status = AcquisitionStatus::Imported {
            file: FileRef {
                path: std::path::PathBuf::from(file),
            },
            quality,
            score: 0,
            at: chrono::Utc::now(),
        };
        store.upsert_edition(&ed).await.unwrap();
    }

    /// SKADI-T-0399: rows adopted before the fix carry the ladder's floor tier.
    /// The backfill re-derives a real tier when the file name carries one and
    /// records Unknown when it does not, so `upgradable()` stops seeing the whole
    /// library as below cutoff. A genuinely graded row is untouched.
    #[tokio::test]
    async fn backfill_regrades_floor_rows_and_leaves_graded_rows_alone() {
        let (_dir, store) = fresh_store().await;
        let defs = skadi_quality::default_definitions();
        let floor = defs[0].id;
        let bluray = defs.iter().find(|d| d.name == "Bluray-1080p").unwrap().id;

        save(
            &store,
            "Parseable",
            "The.Matrix.1999.1080p.BluRay.x264.mkv",
            floor,
        )
        .await;
        save(&store, "Opaque", "movie.mkv", floor).await;
        save(&store, "Graded", "already.graded.mkv", bluray).await;

        let dry = backfill_unassessed_quality(&store, false).await.unwrap();
        assert_eq!(
            (dry.scanned, dry.graded, dry.unknown, dry.kept),
            (3, 1, 1, 1)
        );
        // A dry run changes nothing.
        let after_dry = backfill_unassessed_quality(&store, false).await.unwrap();
        assert_eq!(after_dry, dry);

        let applied = backfill_unassessed_quality(&store, true).await.unwrap();
        assert_eq!(applied, dry);

        // Idempotent: the re-graded row now holds a real tier so it is kept, and
        // the opaque row stays Unknown (its name still says nothing). Nothing is
        // re-graded a second time.
        let again = backfill_unassessed_quality(&store, false).await.unwrap();
        assert_eq!((again.graded, again.unknown, again.kept), (0, 1, 2));
    }
}

#[cfg(test)]
mod probe_tests {
    use super::tests::{fresh_store, save};
    use super::*;

    /// A prober that answers from a fixed map, so the test controls exactly what
    /// each file "is" without needing real media on disk.
    struct StubProber(std::collections::HashMap<String, skadi_core::MediaInfo>);

    impl skadi_media_probe::MediaProber for StubProber {
        fn probe(&self, path: &std::path::Path) -> Option<skadi_core::MediaInfo> {
            self.0.get(path.to_str().unwrap_or_default()).cloned()
        }
    }

    fn hd() -> skadi_core::MediaInfo {
        skadi_core::MediaInfo {
            video: Some(skadi_core::VideoInfo {
                width: 1920,
                height: 1080,
                codec: Some("h264".into()),
                profile: None,
                dynamic_range: None,
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn probing_grades_unknown_rows_and_leaves_assessed_ones_alone() {
        let (_dir, store) = fresh_store().await;
        let defs = skadi_quality::default_definitions();
        let assessed = defs[0].id;

        // One Unknown row the probe can read, one already assessed.
        save(
            &store,
            "Unknown One",
            "/movies/unknown-one/unknown-one.mkv",
            skadi_quality::UNKNOWN_QUALITY_ID,
        )
        .await;
        save(
            &store,
            "Assessed",
            "/movies/assessed/assessed.mkv",
            assessed,
        )
        .await;

        let mut map = std::collections::HashMap::new();
        map.insert("/movies/unknown-one/unknown-one.mkv".to_string(), hd());
        // The assessed row's file would probe fine too — the point is that it is
        // never asked, because a row with real evidence must not be overwritten
        // by an assumed source.
        map.insert("/movies/assessed/assessed.mkv".to_string(), hd());
        let prober = StubProber(map);

        let report = grade_unknown_by_probe(&store, &prober, &defs, true)
            .await
            .unwrap();
        assert_eq!(report.scanned, 1, "only the Unknown row is examined");
        assert_eq!(report.graded, 1);
        assert_eq!(report.kept, 1, "the assessed row is skipped, not re-graded");

        // The assessed row still holds exactly what it held.
        let movies = store.list_movies(MovieFilter::default()).await.unwrap();
        let still = movies
            .iter()
            .find(|m| m.title == "Assessed")
            .and_then(|m| m.editions.first())
            .unwrap();
        let AcquisitionStatus::Imported { quality, .. } = &still.status else {
            panic!("expected Imported")
        };
        assert_eq!(*quality, assessed);
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_probed_stays_unknown() {
        let (_dir, store) = fresh_store().await;
        let defs = skadi_quality::default_definitions();
        save(
            &store,
            "Gone",
            "/movies/gone/gone.mkv",
            skadi_quality::UNKNOWN_QUALITY_ID,
        )
        .await;
        // Empty map: the prober cannot read it — missing, unreadable, or a
        // format it does not know.
        let prober = StubProber(std::collections::HashMap::new());

        let report = grade_unknown_by_probe(&store, &prober, &defs, true)
            .await
            .unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.graded, 0);
        assert_eq!(
            report.unknown, 1,
            "an unprobeable file must stay Unknown, never be assigned a guessed tier"
        );
    }

    #[tokio::test]
    async fn a_dry_run_writes_nothing() {
        let (_dir, store) = fresh_store().await;
        let defs = skadi_quality::default_definitions();
        save(
            &store,
            "Dry",
            "/movies/dry/dry.mkv",
            skadi_quality::UNKNOWN_QUALITY_ID,
        )
        .await;
        let mut map = std::collections::HashMap::new();
        map.insert("/movies/dry/dry.mkv".to_string(), hd());

        let report = grade_unknown_by_probe(&store, &StubProber(map), &defs, false)
            .await
            .unwrap();
        assert_eq!(report.graded, 1, "the report says what a real run would do");

        let movies = store.list_movies(MovieFilter::default()).await.unwrap();
        let AcquisitionStatus::Imported { quality, .. } = &movies[0].editions[0].status else {
            panic!("expected Imported")
        };
        assert!(
            skadi_quality::is_unknown_quality(*quality),
            "a dry run must not write"
        );
    }
}
