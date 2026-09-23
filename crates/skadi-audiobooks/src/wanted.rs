//! `AudiobookWantedQuery` — the hunter's sweep input for the audiobooks domain
//! (SKADI-T-0128), mirroring `skadi_movies::MovieWantedQuery`.
//!
//! Emits one [`AcquireSeed`] per `BookFile` that needs a release:
//! - **`wanted()`** — monitored books with a `Missing` file.
//! - **`upgradable()`** — monitored, `Imported` files below the profile cutoff
//!   rank, when the profile permits upgrades.
//! - **`reconcile_stale()`** — reset files wedged in a non-terminal acquire state
//!   past [`STALE_ACQUIRE_GRACE`](skadi_hunter::STALE_ACQUIRE_GRACE), and restore
//!   `Imported` on `Missing`/`Failed` files whose recorded library file is still
//!   on disk (a demoted import, SKADI-T-0385).
//!
//! Audiobooks search the Torznab **audiobook category (3030)** (e.g. an
//! AudiobookBay indexer behind Jackett/Prowlarr).

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use regex::Regex;

use skadi_core::{AcquisitionStatus, ExternalIds, MediaKind, QualityId, Result};
use skadi_hunter::{AcquireSeed, SearchSpec, WantedQuery};
use skadi_quality::QualityProfile;

use crate::book::Book;
use crate::book_file::BookFile;
use crate::repo::{AudiobooksRepo, BookFilter};

/// Whether the sweep should (re-)acquire a file in this status: `Missing`, or a
/// transfer-`Failed` file whose `retry_at` backoff has elapsed — recovering a
/// flaky source (SKADI-I-0017). A `Failed{retry_at: None}` (no-suitable-release /
/// search failure) is NOT auto-retried.
fn wants_acquire(status: &AcquisitionStatus, now: chrono::DateTime<chrono::Utc>) -> bool {
    match status {
        AcquisitionStatus::Missing => true,
        AcquisitionStatus::Failed {
            retry_at: Some(t), ..
        } => *t <= now,
        _ => false,
    }
}

/// Scoring inputs the sweep needs to decide whether an imported file is below
/// the profile cutoff. Carries a single audiobook quality profile.
#[derive(Clone)]
pub struct WantedScoring {
    pub profile: QualityProfile,
}

/// `WantedQuery` impl for audiobooks.
pub struct AudiobookWantedQuery {
    repo: Arc<dyn AudiobooksRepo>,
    scoring: WantedScoring,
}

impl AudiobookWantedQuery {
    #[must_use]
    pub fn new(repo: Arc<dyn AudiobooksRepo>, scoring: WantedScoring) -> Self {
        Self { repo, scoring }
    }

    /// Build a seed for `file`. `current` is the `(quality, format_score)` it
    /// already holds when this is an *upgrade* seed (`Some` from `upgradable()`),
    /// or `None` for a first acquisition (from `wanted()`) — threaded so the
    /// hunter's `decide` only grabs a strictly-better release on the quality axis
    /// (SKADI-T-0182) or the format-score axis (SKADI-T-0186).
    fn seed_for(
        &self,
        book: &Book,
        file: &BookFile,
        current: Option<(QualityId, i32)>,
    ) -> AcquireSeed {
        AcquireSeed {
            acquirable: file.acquirable_ref(),
            request: SearchSpec {
                // Sweep-driven; the interactive paths override this just before
                // searching (SKADI-T-0539).
                trigger: skadi_hunter::SearchTrigger::Automatic,
                kind: MediaKind::Audiobook,
                titles: build_titles(book),
                year: book.year,
                external_ids: ExternalIds {
                    asin: book.external_ids.asin.clone(),
                    ..Default::default()
                },
                categories: crate::module::AUDIOBOOK_SEARCH_CATEGORIES.to_vec(),
                tv: None,
                series: book.series.as_ref().map(|s| s.name.clone()),
                // Filled in by `steps::search` from the item's tags
                // (SKADI-T-0556) — a database read, so it cannot happen in these
                // pure seed builders.
                tags: None,
            },
            profile: book.profile,
            current_quality: current.map(|(q, _)| q),
            current_format_score: current.map(|(_, s)| s),
            current_unplayable: false,
        }
    }

    fn rank_of(&self, quality_id: QualityId) -> Option<usize> {
        self.scoring
            .profile
            .allowed
            .iter()
            .position(|q| *q == quality_id)
    }

    fn cutoff_rank(&self) -> usize {
        self.scoring
            .profile
            .allowed
            .iter()
            .position(|q| *q == self.scoring.profile.cutoff)
            .unwrap_or(usize::MAX)
    }
}

/// Search titles: the book title, plus a `title author` variant for indexers
/// that need the author to disambiguate (audiobook posts are often
/// `Author - Title`).
fn build_titles(book: &Book) -> Vec<String> {
    let mut out = vec![book.title.clone()];
    if let Some(author) = book.authors.first() {
        out.push(format!("{} {}", book.title, author));
    }
    // Also query the series name (SKADI-T-0313) so a multi-book **pack** is found — searching
    // only the book title never surfaces "Series Books 1-8". `decide` gates singles on the book
    // titles (it excludes the series via `SearchSpec.series`) and packs on the series, so this
    // doesn't loosen per-book precision.
    if let Some(series) = book.series.as_ref() {
        out.push(series.name.clone());
    }
    out
}

/// The `Imported` status to restore for a `Missing`/`Failed` file that still has
/// its library file on disk (SKADI-T-0385), or `None` when the row never recorded
/// a path or the file is gone (a real loss — leave it wanted). Quality/score come
/// from the row's own columns (also written only at import); a row with no
/// quality falls back to the Unknown tier so a later upgrade run can replace it.
async fn restore_demoted_import(file: &BookFile) -> Option<AcquisitionStatus> {
    let path = file.file.as_ref()?.path.clone();
    let probe = path.clone();
    let exists = tokio::task::spawn_blocking(move || probe.is_file())
        .await
        .unwrap_or(false);
    if !exists {
        return None;
    }
    Some(AcquisitionStatus::Imported {
        file: skadi_core::FileRef { path },
        quality: file
            .quality
            .unwrap_or_else(skadi_quality::audiobook::unknown_audiobook_id),
        score: file.format_score,
        at: chrono::Utc::now(),
    })
}

fn imported_path(status: &AcquisitionStatus) -> String {
    match status {
        AcquisitionStatus::Imported { file, .. } => file.path.display().to_string(),
        _ => String::new(),
    }
}

#[async_trait]
impl WantedQuery for AudiobookWantedQuery {
    async fn wanted(&self) -> Result<Vec<AcquireSeed>> {
        let books = self
            .repo
            .list_books(BookFilter {
                monitored: Some(true),
                limit: None,
                offset: None,
            })
            .await?;
        let now = chrono::Utc::now();
        // A *work* the library already holds is satisfied, whichever edition row
        // carries the file (SKADI-T-0400). Audible lists Full Cast / Booktrack /
        // re-issued ASINs as separate products, ingest adds each as its own
        // monitored `books` row, and the sweep then grabbed an audiobook that is
        // already on disk under a sibling ASIN — 10 such rows on the operator's
        // library. Scan every book (not just the monitored ones: the imported
        // sibling may have been unmonitored by hand) and skip the work.
        let satisfied = satisfied_work_keys(
            &self
                .repo
                .list_books(BookFilter {
                    monitored: None,
                    limit: None,
                    offset: None,
                })
                .await?,
        );
        let mut out = Vec::new();
        for book in &books {
            if satisfied.contains(&work_key(book)) {
                continue;
            }
            for file in &book.files {
                // An unmonitored edition is known but not wanted (SKADI-T-0562).
                // Without this the whole decision is cosmetic: a discovered Full
                // Cast edition would be attached "unmonitored" and then acquired
                // by the very next sweep.
                if !file.monitored {
                    continue;
                }
                if wants_acquire(&file.status, now) {
                    // First acquisition: no held quality/score to upgrade over.
                    out.push(self.seed_for(book, file, None));
                }
            }
        }
        // Sweep size (SKADI-T-0456): previously invisible, so "why is it
        // searching hundreds of things?" had no answer short of reading the
        // database. DEBUG — it fires every sweep, and the number only matters
        // when something looks wrong.
        tracing::debug!(
            domain = "audiobooks",
            seeds = out.len(),
            "sweep produced seeds"
        );
        Ok(out)
    }

    async fn reconcile_stale(&self) -> Result<usize> {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE)
                .unwrap_or_else(|_| chrono::Duration::seconds(900));
        let books = self
            .repo
            .list_books(BookFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await?;
        let mut recovered = 0;
        for book in &books {
            for file in &book.files {
                // Heal a demoted import (SKADI-T-0385): `file_path` is written ONLY by an
                // `Imported` status write, so a `Missing`/`Failed` row that still carries a
                // path was imported once and later overwritten by a duplicate acquire run
                // (SKADI-T-0388). If the file is still on disk the library holds it —
                // restore `Imported` rather than re-downloading (and re-failing) forever.
                if matches!(
                    file.status,
                    AcquisitionStatus::Missing | AcquisitionStatus::Failed { .. }
                ) {
                    if let Some(imported) = restore_demoted_import(file).await {
                        tracing::warn!(
                            book_file = %file.id,
                            path = %imported_path(&imported),
                            "restoring demoted audiobook import (file still on disk)"
                        );
                        self.repo.set_book_file_status(file.id, imported).await?;
                        recovered += 1;
                    }
                    continue;
                }
                let in_flight = matches!(
                    file.status,
                    AcquisitionStatus::Searching { .. }
                        | AcquisitionStatus::Snatched { .. }
                        | AcquisitionStatus::Downloading { .. }
                );
                if in_flight && file.updated_at < cutoff {
                    // A wedged row that still has its library file on disk is the same
                    // demoted import (a duplicate run stalled after overwriting `Imported`):
                    // restore it directly rather than bouncing through `Missing`, which
                    // would let `wanted()` re-grab it in this very sweep.
                    let next = match restore_demoted_import(file).await {
                        Some(imported) => {
                            tracing::warn!(
                                book_file = %file.id,
                                stuck_since = %file.updated_at,
                                path = %imported_path(&imported),
                                "recovering wedged audiobook file (file on disk → Imported)"
                            );
                            imported
                        }
                        None => {
                            tracing::warn!(
                                book_file = %file.id,
                                stuck_since = %file.updated_at,
                                "recovering wedged audiobook file (reset to Missing)"
                            );
                            AcquisitionStatus::Missing
                        }
                    };
                    self.repo.set_book_file_status(file.id, next).await?;
                    recovered += 1;
                }
            }
        }
        Ok(recovered)
    }

    async fn upgradable(&self) -> Result<Vec<AcquireSeed>> {
        if !self.scoring.profile.upgrade_allowed {
            return Ok(Vec::new());
        }
        let cutoff_rank = self.cutoff_rank();
        let books = self
            .repo
            .list_books(BookFilter {
                monitored: Some(true),
                limit: None,
                offset: None,
            })
            .await?;
        let mut out = Vec::new();
        for book in &books {
            for file in &book.files {
                // Unmonitored editions are not upgraded either (SKADI-T-0562) —
                // an edition nobody asked for should not quietly start pulling
                // better copies of itself.
                if !file.monitored {
                    continue;
                }
                // CONSUMED INTERFACE (skadi_core): AcquisitionStatus::Imported
                // carries the held `quality: QualityId` and aggregate format
                // `score: i32`; both thread into the seed so the hunter upgrades
                // on the quality axis (SKADI-T-0182) or the format-score axis
                // (SKADI-T-0186).
                let (qid, fscore) = match &file.status {
                    AcquisitionStatus::Imported { quality, score, .. } => (*quality, *score),
                    _ => continue,
                };
                // Unknown is a real row in the audiobook ladder (rank 0, so a known
                // low-bitrate file still upgrades), which means it cannot be skipped
                // by rank alone — special-case it: an unassessed file is not an
                // upgrade candidate (SKADI-T-0399). A quality outside the profile is
                // likewise not judgeable against the cutoff.
                if skadi_quality::audiobook::unknown_audiobook_id() == qid {
                    continue;
                }
                let rank = self.rank_of(qid).unwrap_or(0);
                if rank < cutoff_rank {
                    out.push(self.seed_for(book, file, Some((qid, fscore))));
                }
            }
        }
        // Sweep size (SKADI-T-0456): previously invisible, so "why is it
        // searching hundreds of things?" had no answer short of reading the
        // database. DEBUG — it fires every sweep, and the number only matters
        // when something looks wrong.
        tracing::debug!(
            domain = "audiobooks",
            seeds = out.len(),
            "sweep produced seeds"
        );
        Ok(out)
    }
}

/// The identity of a *work*, for deciding whether the library already holds it
/// under a different edition (SKADI-T-0400): normalized title + primary author.
///
/// Audible ships the same book as several products — `Blood of Elves`,
/// `Blood of Elves (Full Cast Edition)`, `The Tower of Swallows: Booktrack
/// Edition`, plus straight re-issues under a new ASIN — and ingest adds each as
/// its own `books` row. Edition qualifiers are stripped so those collapse onto
/// one key; everything else (subtitle, series position) is left alone so genuinely
/// different books never collide.
pub(crate) fn work_key(book: &Book) -> String {
    let title = book.title.to_ascii_lowercase();
    // Cut a trailing edition qualifier, whether parenthesised or after a colon:
    // "(full cast edition)", ": booktrack edition", "(dramatized adaptation)", …
    let title = EDITION_SUFFIX_RE.replace_all(&title, "");
    let author = book.authors.first().map(String::as_str).unwrap_or_default();
    format!("{}|{}", alnum_key(&title), alnum_key(author))
}

/// Lowercase, ASCII-alphanumeric-only key (drops punctuation and spacing), the
/// same shape `discovery::name_key` uses for author matching.
pub(crate) fn alnum_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Trailing edition qualifiers Audible appends to a title.
pub(crate) static EDITION_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s*[:(\[]\s*(full[ -]cast|booktrack|dramatized|dramatised|abridged|unabridged|audible|special|anniversary|collector'?s|deluxe)[^)\]]*[)\]]?\s*$",
    )
    .expect("static regex")
});

/// The work keys the library already holds: any book with an `Imported` file.
fn satisfied_work_keys(books: &[Book]) -> std::collections::HashSet<String> {
    books
        .iter()
        .filter(|b| {
            b.files
                .iter()
                .any(|f| matches!(f.status, AcquisitionStatus::Imported { .. }))
        })
        .map(work_key)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use diesel::connection::Connection;
    use diesel::sqlite::SqliteConnection;
    use diesel_migrations::MigrationHarness;

    use skadi_core::{AsinId, FileRef, ProfileId, RootFolder};
    use skadi_quality::audiobook::{default_audiobook_definitions, unknown_audiobook_id};
    use skadi_store::Store;

    use crate::SQLITE_MIGRATIONS;

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

    /// A 2-rank audiobook profile: MP3-64 (low) < M4B-256 (high), cutoff high.
    fn audiobook_profile() -> (QualityProfile, QualityId, QualityId) {
        let defs = default_audiobook_definitions();
        let lo = defs.iter().find(|d| d.name == "MP3-64").unwrap().id;
        let hi = defs.iter().find(|d| d.name == "M4B-256").unwrap().id;
        (
            QualityProfile {
                id: ProfileId::new(),
                name: "ab".into(),
                allowed: vec![lo, hi],
                cutoff: hi,
                upgrade_allowed: true,
                formats: vec![],
                min_format_score: 0,
            },
            lo,
            hi,
        )
    }

    async fn seed_book(store: &Store, asin: &str, monitored: bool, status: AcquisitionStatus) {
        let mut book = Book::new(
            ExternalIds {
                asin: Some(AsinId(asin.into())),
                ..Default::default()
            },
            "The Way of Kings",
            ProfileId::new(),
            RootFolder::new("/audiobooks"),
        );
        book.monitored = monitored;
        book.year = Some(2010);
        book.authors = vec!["Brandon Sanderson".into()];
        store.upsert_book(&book).await.unwrap();
        let mut file = BookFile::missing(book.id);
        file.status = status;
        store.upsert_book_file(&file).await.unwrap();
    }

    /// SKADI-T-0400: Audible sells the same work as several products. A title the
    /// library already holds must not be wanted again under a sibling ASIN, and
    /// genuinely different books must not collapse onto one key.
    #[test]
    fn edition_siblings_share_a_work_key() {
        let book = |title: &str, author: &str| {
            let mut b = Book::new(
                ExternalIds::default(),
                title,
                ProfileId::new(),
                RootFolder::new("/audiobooks"),
            );
            b.authors = vec![author.to_string()];
            b
        };
        let base = work_key(&book("Blood of Elves", "Andrzej Sapkowski"));
        for variant in [
            "Blood of Elves (Full Cast Edition)",
            "Blood of Elves: Booktrack Edition",
            "blood of elves",
            "Blood of Elves (Dramatized Adaptation)",
        ] {
            assert_eq!(
                work_key(&book(variant, "Andrzej Sapkowski")),
                base,
                "{variant} should be the same work"
            );
        }
        assert_ne!(
            work_key(&book("Sword of Destiny", "Andrzej Sapkowski")),
            base
        );
        assert_ne!(work_key(&book("Blood of Elves", "Someone Else")), base);
    }

    #[tokio::test]
    async fn wanted_returns_missing_files_on_monitored_books_only() {
        let (_d, store) = fresh_store().await;
        let (profile, _, _) = audiobook_profile();
        seed_book(&store, "B1", true, AcquisitionStatus::Missing).await;
        seed_book(&store, "B2", false, AcquisitionStatus::Missing).await;

        let q = AudiobookWantedQuery::new(Arc::new(store), WantedScoring { profile });
        let seeds = q.wanted().await.unwrap();
        assert_eq!(seeds.len(), 1, "only the monitored book is wanted");
        // First acquisition: no held quality to upgrade over (SKADI-T-0182).
        assert_eq!(seeds[0].current_quality, None);
        assert_eq!(seeds[0].request.kind, MediaKind::Audiobook);
        assert_eq!(
            seeds[0].request.categories,
            crate::module::AUDIOBOOK_SEARCH_CATEGORIES.to_vec()
        );
        assert_eq!(seeds[0].request.year, Some(2010));
        assert!(
            seeds[0]
                .request
                .titles
                .iter()
                .any(|t| t.contains("Brandon Sanderson")),
            "author-qualified title alias present"
        );
    }

    #[tokio::test]
    async fn wanted_re_acquires_failed_only_after_retry_at_backoff() {
        let (_d, store) = fresh_store().await;
        let (profile, _, _) = audiobook_profile();
        let now = chrono::Utc::now();
        // Transfer-failed, backoff elapsed → re-acquired (flaky-source recovery).
        seed_book(
            &store,
            "PAST",
            true,
            AcquisitionStatus::Failed {
                reason: skadi_core::FailureReason::DownloadFailed("flaky".into()),
                retry_at: Some(now - chrono::Duration::minutes(1)),
                attempts: 0,
            },
        )
        .await;
        // Transfer-failed, still backing off → NOT yet wanted.
        seed_book(
            &store,
            "FUTURE",
            true,
            AcquisitionStatus::Failed {
                reason: skadi_core::FailureReason::DownloadFailed("flaky".into()),
                retry_at: Some(now + chrono::Duration::hours(1)),
                attempts: 0,
            },
        )
        .await;
        // No-suitable-release (retry_at: None) → never auto-retried.
        seed_book(
            &store,
            "TERMINAL",
            true,
            AcquisitionStatus::Failed {
                reason: skadi_core::FailureReason::NoSuitableRelease,
                retry_at: None,
                attempts: 0,
            },
        )
        .await;

        let q = AudiobookWantedQuery::new(Arc::new(store), WantedScoring { profile });
        let seeds = q.wanted().await.unwrap();
        assert_eq!(seeds.len(), 1, "only the elapsed-backoff failure is wanted");
        assert!(
            seeds[0]
                .request
                .external_ids
                .asin
                .as_ref()
                .map(|a| a.0.as_str())
                == Some("PAST"),
            "the re-acquired book is the elapsed-backoff one"
        );
    }

    #[tokio::test]
    async fn reconcile_stale_resets_wedged_files_past_the_grace() {
        let (_d, store) = fresh_store().await;
        let (profile, _, _) = audiobook_profile();

        async fn seed_downloading(store: &Store, asin: &str, updated_at: chrono::DateTime<Utc>) {
            let mut book = Book::new(
                ExternalIds {
                    asin: Some(AsinId(asin.into())),
                    ..Default::default()
                },
                "WoK",
                ProfileId::new(),
                RootFolder::new("/audiobooks"),
            );
            book.year = Some(2010);
            store.upsert_book(&book).await.unwrap();
            let mut file = BookFile::missing(book.id);
            file.status = AcquisitionStatus::Downloading {
                release: skadi_core::ReleaseId::new(),
                progress: 0.0,
            };
            file.updated_at = updated_at;
            store.upsert_book_file(&file).await.unwrap();
        }

        seed_downloading(
            &store,
            "STALE",
            Utc::now()
                - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE).unwrap()
                - chrono::Duration::minutes(5),
        )
        .await;
        seed_downloading(&store, "FRESH", Utc::now()).await;

        let q = AudiobookWantedQuery::new(Arc::new(store.clone()), WantedScoring { profile });
        let recovered = q.reconcile_stale().await.unwrap();
        assert_eq!(recovered, 1, "only the wedged file is recovered");

        // The reset file now shows up as wanted.
        let stale_book = store
            .get_book_by_asin(&AsinId("STALE".into()))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stale_book.files[0].status,
            AcquisitionStatus::Missing
        ));
        let fresh_book = store
            .get_book_by_asin(&AsinId("FRESH".into()))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            fresh_book.files[0].status,
            AcquisitionStatus::Downloading { .. }
        ));
    }

    /// SKADI-T-0385: a `Failed`/`Missing` row that still carries the library path
    /// of a file on disk was imported once and demoted by a duplicate run — it is
    /// restored to `Imported` (quality/score from the row) and no longer wanted.
    /// A row whose path is gone, or that never had one, is left alone.
    #[tokio::test]
    async fn reconcile_stale_restores_demoted_imports_whose_file_exists() {
        let (_d, store) = fresh_store().await;
        let (profile, lo, _) = audiobook_profile();
        let lib = tempfile::tempdir().unwrap();
        let present = lib.path().join("present.m4b");
        std::fs::write(&present, b"audio").unwrap();
        let gone = lib.path().join("gone.m4b");

        async fn seed(
            store: &Store,
            asin: &str,
            status: AcquisitionStatus,
            path: Option<std::path::PathBuf>,
            quality: Option<QualityId>,
        ) {
            let mut book = Book::new(
                ExternalIds {
                    asin: Some(AsinId(asin.into())),
                    ..Default::default()
                },
                // Distinct titles: these are four different works, so they must not
                // collapse onto one work key — a title the library already holds is
                // satisfied whichever edition row carries the file (SKADI-T-0400).
                format!("WoK {asin}"),
                ProfileId::new(),
                RootFolder::new("/audiobooks"),
            );
            book.monitored = true;
            store.upsert_book(&book).await.unwrap();
            let mut file = BookFile::missing(book.id);
            file.status = status;
            file.file = path.map(|p| FileRef { path: p });
            file.quality = quality;
            file.format_score = 7;
            store.upsert_book_file(&file).await.unwrap();
        }

        let failed = AcquisitionStatus::Failed {
            reason: skadi_core::FailureReason::ImportFailed("no placed files".into()),
            retry_at: Some(Utc::now() - chrono::Duration::minutes(1)),
            attempts: 3,
        };
        // Demoted with the file present + a recorded quality → restored as-is.
        seed(
            &store,
            "PRESENT",
            failed.clone(),
            Some(present.clone()),
            Some(lo),
        )
        .await;
        // Demoted, present, but no quality column → restored at the Unknown tier.
        seed(
            &store,
            "NOQUAL",
            AcquisitionStatus::Missing,
            Some(present.clone()),
            None,
        )
        .await;
        // Path recorded but the file is gone → a real loss, stays wanted.
        seed(&store, "GONE", failed.clone(), Some(gone), Some(lo)).await;
        // Never imported → untouched.
        seed(&store, "FRESH", AcquisitionStatus::Missing, None, None).await;
        // Wedged in-flight (stale Downloading) with the file on disk → restored to
        // Imported directly, not bounced through Missing.
        seed(
            &store,
            "WEDGED",
            AcquisitionStatus::Downloading {
                release: skadi_core::ReleaseId::new(),
                progress: 0.4,
            },
            Some(present.clone()),
            Some(lo),
        )
        .await;
        {
            // Backdate past the grace so it counts as wedged.
            let mut b = store
                .get_book_by_asin(&AsinId("WEDGED".into()))
                .await
                .unwrap()
                .unwrap();
            b.files[0].updated_at = Utc::now()
                - chrono::Duration::from_std(skadi_hunter::STALE_ACQUIRE_GRACE).unwrap()
                - chrono::Duration::minutes(5);
            store.upsert_book_file(&b.files[0]).await.unwrap();
        }

        let q = AudiobookWantedQuery::new(Arc::new(store.clone()), WantedScoring { profile });
        let recovered = q.reconcile_stale().await.unwrap();
        assert_eq!(
            recovered, 3,
            "both on-disk demotions + the wedged row are healed"
        );
        assert!(matches!(
            store
                .get_book_by_asin(&AsinId("WEDGED".into()))
                .await
                .unwrap()
                .unwrap()
                .files[0]
                .status,
            AcquisitionStatus::Imported { .. }
        ));

        let status_of = |asin: &str| {
            let store = store.clone();
            let asin = AsinId(asin.into());
            async move {
                store.get_book_by_asin(&asin).await.unwrap().unwrap().files[0]
                    .status
                    .clone()
            }
        };
        match status_of("PRESENT").await {
            AcquisitionStatus::Imported {
                file,
                quality,
                score,
                ..
            } => {
                assert_eq!(file.path, present);
                assert_eq!(quality, lo);
                assert_eq!(score, 7);
            }
            other => panic!("PRESENT should be Imported, got {other:?}"),
        }
        match status_of("NOQUAL").await {
            AcquisitionStatus::Imported { quality, .. } => {
                assert_eq!(quality, unknown_audiobook_id());
            }
            other => panic!("NOQUAL should be Imported, got {other:?}"),
        }
        assert!(matches!(
            status_of("GONE").await,
            AcquisitionStatus::Failed { .. }
        ));
        assert!(matches!(
            status_of("FRESH").await,
            AcquisitionStatus::Missing
        ));

        // The healed rows have left the wanted set; the real loss is still in it.
        let wanted: Vec<String> = q
            .wanted()
            .await
            .unwrap()
            .into_iter()
            .filter_map(|s| s.request.external_ids.asin.map(|a| a.0))
            .collect();
        assert!(wanted.contains(&"GONE".to_string()));
        assert!(wanted.contains(&"FRESH".to_string()));
        assert!(!wanted.contains(&"PRESENT".to_string()));
        assert!(!wanted.contains(&"NOQUAL".to_string()));
    }

    #[tokio::test]
    async fn upgradable_respects_cutoff_and_profile_flag() {
        let (_d, store) = fresh_store().await;
        let (profile, lo, hi) = audiobook_profile();

        // Imported at MP3-64 (below the M4B-256 cutoff) → upgradable.
        seed_book(
            &store,
            "LOW",
            true,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/audiobooks/x.mp3".into(),
                },
                quality: lo,
                score: 0,
                at: Utc::now(),
            },
        )
        .await;
        let q = AudiobookWantedQuery::new(
            Arc::new(store.clone()),
            WantedScoring {
                profile: profile.clone(),
            },
        );
        let up = q.upgradable().await.unwrap();
        assert_eq!(up.len(), 1);
        // The upgrade seed carries the held quality + format score so the hunter's
        // `decide` upgrades on either axis (SKADI-T-0182 / SKADI-T-0186).
        assert_eq!(up[0].current_quality, Some(lo));
        assert_eq!(up[0].current_format_score, Some(0));

        // Imported at the cutoff → not upgradable.
        let (_d2, store2) = fresh_store().await;
        seed_book(
            &store2,
            "HIGH",
            true,
            AcquisitionStatus::Imported {
                file: FileRef {
                    path: "/audiobooks/x.m4b".into(),
                },
                quality: hi,
                score: 0,
                at: Utc::now(),
            },
        )
        .await;
        let q2 = AudiobookWantedQuery::new(Arc::new(store2), WantedScoring { profile });
        assert!(q2.upgradable().await.unwrap().is_empty());
    }
}
