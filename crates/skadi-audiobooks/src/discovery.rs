//! Author-monitoring discovery (SKADI-T-0132).
//!
//! For each **monitored** [`Author`](crate::author::Author), list that author's
//! Audible catalog ([`AudibleCatalogProvider`]) and add any product not already
//! in the library as a monitored `Missing` [`Book`](crate::book::Book) (enriched
//! through the existing Audnexus [`add_book`](crate::add_book) path). The hunter's
//! normal sweep then acquires the new `BookFile`. This is what makes the
//! Author→Series→Book hierarchy worth having: a monitored author auto-acquires
//! new releases.
//!
//! **Why a plain interval worker, not a Cloacina workflow** (cf. the movie
//! metadata-refresh worker): discovery is naturally idempotent — `add_book`
//! refuses duplicate ASINs and we pre-check `get_book_by_asin` — and cheap, so it
//! needs no crash-resume/retry machinery. A missed pass is simply picked up by
//! the next tick. Each pass is bounded by [`MAX_NEW_PER_PASS`] so a prolific
//! author can't flood the library (or the metadata upstreams) in one tick.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, NaiveDate};
use tokio_util::sync::CancellationToken;

use skadi_core::module::BoxFuture;
use skadi_core::{AppError, AsinId, ProfileId, Result, RootFolder, Worker};
use skadi_metadata::{AudibleCatalogProvider, CatalogItem, MAX_PAGES_PER_AUTHOR, MetadataProvider};
use skadi_store::Store;

use crate::author::Author;
use crate::metadata::add_book;
use crate::repo::{AudiobooksRepo, AuthorFilter, BookFilter, WatchersRepo, WorksRepo};
use crate::work::{WatchScope, Work};

/// Default cadence for the discovery pass — authors release infrequently, so an
/// hourly-ish sweep is plenty (tunable later via the config plane).
pub const DEFAULT_DISCOVERY_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// Catalog **pages** read per periodic pass, across all authors, so a big library
/// cannot hammer the Audible catalog in one tick.
///
/// This used to be a cap of 50 *authors*, which assumed one request per author.
/// Reading a whole catalog costs several requests for a big author (SKADI-T-0650),
/// so the budget is now counted in what it actually spends. An author cut off
/// mid-catalog resumes at the next page on the following pass.
pub const MAX_PAGES_PER_PASS: u32 = 60;

/// Where the next periodic pass picks up (SKADI-T-0650).
///
/// Held in memory: a restart simply begins again from the first author at page 0,
/// which is safe because ingest upserts. Persisting it would buy a slightly
/// faster first pass after a restart in exchange for a migration.
#[derive(Default)]
struct IngestCursor {
    /// The author name the next pass starts at. Targets are walked in **name
    /// order** — they used to come out of a `HashMap`, whose order changes every
    /// pass, so with a per-pass cap some authors went unvisited for many passes.
    next_author: Option<String>,
    /// Page to resume at for an author whose read was cut off by the budget.
    resume_page: std::collections::HashMap<String, u32>,
}

/// What one bounded read of an author's catalog achieved.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IngestProgress {
    /// Works upserted.
    pub upserted: usize,
    /// The author's own ASIN, when it could be resolved (see [`ingest_author_works`]).
    pub resolved: Option<AsinId>,
    /// Pages actually requested.
    pub pages_read: u32,
    /// Products the name search returned that do **not** credit this author and
    /// so were not ingested for them (SKADI-T-0653). Audible's `author=` search
    /// is fuzzy: "George R. R. Martin" also returns George R. Martin III.
    pub foreign: usize,
    /// `Some(page)` when the catalog has more and the read stopped at its page
    /// allowance; `None` when the author's catalog was read to the end.
    pub next_page: Option<u32>,
}

/// Upper bound on books queued for acquisition per watcher-resolution pass, so a
/// freshly-watched prolific author/series can't flood the hunter in one tick.
const MAX_ACQUIRE_PER_PASS: usize = 25;

/// Ingest one author's **whole** Audible catalog into the known-works store
/// (SKADI-I-0018): every page, up to [`MAX_PAGES_PER_AUTHOR`]. Acquires nothing.
/// Used where the caller wants one author finished now — ingest-on-add, and the
/// operator's "Refresh catalog". The periodic pass uses
/// [`ingest_author_pages`] so it can spread a big author across passes.
///
/// Returns `(works upserted, resolved primary-author ASIN)`. The resolved ASIN is
/// the registered `author_asin` when given, else the one the catalog reports for a
/// product whose primary author matches `author_name` — the caller uses it to
/// register an author entity for an imported library (SKADI-T-0160).
pub async fn ingest_author_works(
    store: &Store,
    catalog: &AudibleCatalogProvider,
    author_name: &str,
    author_asin: Option<&AsinId>,
) -> Result<(usize, Option<AsinId>)> {
    let p = ingest_author_pages(
        store,
        catalog,
        author_name,
        author_asin,
        0,
        MAX_PAGES_PER_AUTHOR,
    )
    .await?;
    if p.next_page.is_some() {
        tracing::warn!(
            author = author_name,
            pages = MAX_PAGES_PER_AUTHOR,
            "catalog ingest: page ceiling reached; the rest of this author's catalog was not read"
        );
    }
    Ok((p.upserted, p.resolved))
}

/// Read up to `max_pages` pages of an author's catalog starting at page `start`,
/// upserting each page's works as it arrives — so a failure partway keeps what was
/// already read (SKADI-T-0650).
///
/// Language comes **inline** from the list response when present; the per-title
/// lookup is only the fallback. Before SKADI-T-0650 every unclassified product
/// cost an extra request here.
pub async fn ingest_author_pages(
    store: &Store,
    catalog: &AudibleCatalogProvider,
    author_name: &str,
    author_asin: Option<&AsinId>,
    start: u32,
    max_pages: u32,
) -> Result<IngestProgress> {
    let mut progress = IngestProgress {
        resolved: author_asin.cloned(),
        ..IngestProgress::default()
    };
    let want = name_key(author_name);
    // For an unregistered target: how often each ASIN is credited under this
    // name. The *most* credited wins, not the first seen — on a newest-first
    // list the first match is whatever was published last, which is how a
    // bio-less duplicate author page became a second George R. R. Martin
    // (SKADI-T-0653).
    let mut votes: std::collections::HashMap<AsinId, usize> = std::collections::HashMap::new();
    let mut index = start;
    loop {
        if progress.pages_read >= max_pages {
            progress.next_page = Some(index);
            return Ok(progress);
        }
        let page = catalog.list_by_author_page(author_name, index).await?;
        progress.pages_read += 1;
        let last = page.is_last(index);

        let mut works: Vec<Work> = Vec::with_capacity(page.items.len());
        for item in &page.items {
            let Some(attributed) = credited_asin(item, &want, author_asin) else {
                progress.foreign += 1;
                continue;
            };
            if author_asin.is_none()
                && let Some(a) = &attributed
            {
                *votes.entry(a.clone()).or_insert(0) += 1;
            }
            let mut w = work_from_catalog(item, attributed);
            w.language = match &item.language {
                Some(l) => Some(l.clone()),
                None => classify_language(store, catalog, &w.asin).await,
            };
            // English-only catalog (SKADI-I-0051): a big Audible catalog is full of
            // foreign-language editions (German/French/Spanish translations). Don't
            // store the positively-non-English ones at all, so the catalog + every
            // downstream (series rollups, watcher acquisition) is English-only.
            if crate::work::is_catalog_language(w.language.as_deref()) {
                works.push(w);
            }
        }
        store.upsert_works(&works).await?;
        progress.upserted += works.len();

        if author_asin.is_none() {
            progress.resolved = votes
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.0.cmp(&a.0.0)))
                .map(|(asin, _)| asin.clone());
        }

        if last {
            return Ok(progress);
        }
        index += 1;
    }
}

/// Whether `item` is really by the author discovery searched for, and if so
/// which ASIN to attribute it to (SKADI-T-0653).
///
/// Audible's `author=` search is fuzzy, so the results include other people:
/// searching "George R. R. Martin" returns George R. Martin III's books, under a
/// different ASIN. Attributing every result to the searched author put those
/// books in his body of work — and, through an author watcher, in line to be
/// downloaded.
///
/// - **Registered author (ASIN known).** Credited when a contributor carries
///   that ASIN. When a contributor has the matching *name* but **no** ASIN — a
///   translated edition often drops it — that name match counts too. A matching
///   name under a *different* ASIN is somebody else, and does not.
/// - **Unregistered target (no ASIN).** Credited when a contributor's name key
///   matches; attributed to that contributor's ASIN, if any.
///
/// - **A product crediting nobody** keeps the searched author: the search
///   result is the only evidence there is, and nothing contradicts it.
///
/// `None` = not this author's product; it is not ingested for them.
///
/// The rule itself is [`credits`], which the startup link repair also uses on
/// stored works, so the two cannot disagree (SKADI-T-0656).
pub(crate) fn credited_asin(
    item: &CatalogItem,
    want_key: &str,
    author_asin: Option<&AsinId>,
) -> Option<Option<AsinId>> {
    credits(&item.authors, &item.author_asins, want_key, author_asin)
}

/// The attribution rule over a contributor list: `authors` with their ASINs,
/// aligned by index (a missing or short `author_asins` means "no ASIN"). See
/// [`credited_asin`] for the rule; this is the one place it lives.
pub(crate) fn credits(
    authors: &[String],
    author_asins: &[Option<AsinId>],
    want_key: &str,
    author_asin: Option<&AsinId>,
) -> Option<Option<AsinId>> {
    if authors.is_empty() {
        return Some(author_asin.cloned());
    }
    let mut contributors = authors
        .iter()
        .zip(author_asins.iter().chain(std::iter::repeat(&None)));
    match author_asin {
        Some(asin) => {
            if author_asins.iter().flatten().any(|a| a == asin) {
                return Some(Some(asin.clone()));
            }
            contributors
                .filter(|(name, _)| name_key(name) == want_key)
                .any(|(_, a)| a.is_none())
                .then(|| Some(asin.clone()))
        }
        None => contributors
            .find(|(name, _)| name_key(name) == want_key)
            .map(|(_, a)| a.clone()),
    }
}

/// The work's Audible language, lowercased — reused from the cached
/// [`Work::language`] when already classified, else fetched once from the Audible
/// per-title detail endpoint (the list endpoints don't carry it). `None` on a
/// lookup miss/failure, which the catalog views treat as "show" (SKADI-T-0160).
async fn classify_language(
    store: &Store,
    catalog: &AudibleCatalogProvider,
    asin: &AsinId,
) -> Option<String> {
    if let Ok(Some(existing)) = store.get_work(asin).await
        && let Some(lang) = existing.language
    {
        return Some(lang);
    }
    catalog.product_language(asin).await.unwrap_or(None)
}

/// A normalized key for matching author display names across sources — lowercase,
/// ASCII-alphanumeric only (drops '.', spacing, punctuation). Lets "A.G. Riddle",
/// "A. G. Riddle" and "a g riddle" compare equal without over-eager fuzzy matching.
///
/// A known contributor-role suffix is dropped first (SKADI-T-0652), so a name
/// stored before roles were parsed at ingest still keys as the person.
pub(crate) fn name_key(name: &str) -> String {
    let (name, _) = skadi_metadata::split_role(name);
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Build a [`Work`] from an Audible [`CatalogItem`] + the owning author's ASIN.
pub(crate) fn work_from_catalog(item: &CatalogItem, attributed: Option<AsinId>) -> Work {
    let mut w = Work::new(item.asin.clone(), item.title.clone());
    w.authors = item.authors.clone();
    // Kept so the link repair can judge the work by ASIN, as discovery did,
    // rather than by name (SKADI-T-0656).
    w.author_asins = item.author_asins.clone();
    // Attribution is decided by [`credited_asin`], from the product's own
    // contributors. It used to *prefer the searched author's ASIN* over the
    // product's, which credited every product a fuzzy name search returned to
    // whoever was searched (SKADI-T-0653).
    w.author_asin = attributed;
    w.series_name = item.series_name.clone();
    w.series_asin = item.series_asin.clone();
    w.series_position = item.series_position.clone();
    w.cover_url = item.cover_url.clone();
    w.release_date = item.release_date.map(|d| d.format("%Y-%m-%d").to_string());
    w
}

/// Rank library-driven discoveries (SKADI-T-0317): drop works already owned, then order
/// **upcoming (future `release_date`, soonest first) → recently released (newest first) →
/// undated/unparseable**. Pure; `today` fixes "now" for the caller and tests. `owned` is the set
/// of owned ASIN strings (a work is owned iff a library book carries its ASIN).
#[must_use]
pub fn rank_discoveries(works: Vec<Work>, owned: &HashSet<String>, today: NaiveDate) -> Vec<Work> {
    let mut unowned: Vec<Work> = works
        .into_iter()
        .filter(|w| !owned.contains(&w.asin.0))
        .collect();
    unowned.sort_by_key(|w| disco_key(w, today));
    unowned
}

/// Sort key for [`rank_discoveries`] — ascending order yields upcoming-soonest, then
/// most-recently-released, then undated. Band 0 = upcoming (asc by date); band 1 = released
/// (newest first via negated day count); band 2 = undated/unparseable.
fn disco_key(w: &Work, today: NaiveDate) -> (u8, i64) {
    match w
        .release_date
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
    {
        Some(d) if d >= today => (0, i64::from(d.num_days_from_ce())),
        Some(d) => (1, -i64::from(d.num_days_from_ce())),
        None => (2, 0),
    }
}

/// Discovers new books for monitored authors. Holds the repo, the Audible catalog
/// provider (author → product ASINs), and the enrichment metadata provider
/// (per-ASIN records — Audnexus); profile/root defaults are resolved from the
/// settings each pass so config changes take effect without a restart.
pub struct AuthorDiscovery {
    repo: Arc<dyn AudiobooksRepo>,
    store: Store,
    catalog: Arc<AudibleCatalogProvider>,
    enrich: Arc<dyn MetadataProvider>,
    /// Rotation and resume state between periodic passes. It persists because the
    /// worker holds one `Arc<AuthorDiscovery>` for its lifetime (`module.rs`) and
    /// calls [`run_once`](Self::run_once) every tick.
    ///
    /// The mutex serialises passes **on this instance** only. The operator's
    /// "Refresh catalog" builds its own `AuthorDiscovery` per request, with its own
    /// cursor, so it can run alongside a periodic pass; both upsert, so the cost
    /// of overlap is duplicate requests, not bad data.
    cursor: tokio::sync::Mutex<IngestCursor>,
}

impl AuthorDiscovery {
    #[must_use]
    pub fn new(
        repo: Arc<dyn AudiobooksRepo>,
        store: Store,
        catalog: Arc<AudibleCatalogProvider>,
        enrich: Arc<dyn MetadataProvider>,
    ) -> Self {
        Self {
            repo,
            store,
            catalog,
            enrich,
            cursor: tokio::sync::Mutex::new(IngestCursor::default()),
        }
    }

    /// One periodic pass: ingest catalogs into known-works (bounded per pass so the
    /// auto-sweep stays gentle), then resolve watchers into acquisitions
    /// (SKADI-I-0018). Returns (works upserted, books acquired).
    pub async fn run_once(&self) -> (usize, usize) {
        let ingested = self.ingest_once(MAX_PAGES_PER_PASS).await;
        let acquired = self.resolve_watchers_once().await;
        (ingested, acquired)
    }

    /// A **complete** pass over *every* library author — backs the operator's
    /// "Refresh catalog" action (SKADI-T-0160). Uncapped because it's explicitly
    /// user-triggered and we want it to finish the job in one go (already-classified
    /// works skip the per-title language lookup, so re-runs are cheap).
    pub async fn run_full(&self) -> (usize, usize) {
        let ingested = self.ingest_once(u32::MAX).await;
        let acquired = self.resolve_watchers_once().await;
        (ingested, acquired)
    }

    /// Ingest each known author's works into the **known-works** store (know,
    /// don't acquire). Returns the number of works upserted. Also auto-migrates a
    /// legacy `monitored` author into an author-level watcher (SKADI-T-0156), so
    /// existing monitored authors keep acquiring once watchers drive acquisition.
    pub async fn ingest_once(&self, max_pages: u32) -> usize {
        // Ingest targets are keyed by author **name** (the catalog query is by
        // name). Registered authors contribute their ASIN (so their works carry
        // `author_asin` for the author-page browse); library books contribute any
        // author name that has no registered author — this is what makes an
        // *imported* library (books, no authors) get series/body-of-work data
        // (SKADI-T-0160).
        let mut targets: std::collections::HashMap<String, Option<AsinId>> =
            std::collections::HashMap::new();

        match self.repo.list_authors(AuthorFilter::default()).await {
            Ok(authors) => {
                for author in authors {
                    targets.insert(author.name.clone(), author.asin.clone());
                    // One-time legacy migration: a `monitored` author (with an
                    // ASIN) becomes an author watcher and is then un-monitored, so
                    // the watcher is the sole acquisition signal (and an operator
                    // un-watch sticks).
                    if author.monitored
                        && let Some(asin) = author.asin.as_ref()
                        && self
                            .store
                            .set_watcher(WatchScope::Author, &asin.0)
                            .await
                            .is_ok()
                    {
                        let mut a = author.clone();
                        a.monitored = false;
                        let _ = self.repo.upsert_author(&a).await;
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "catalog ingest: listing authors failed"),
        }

        // A library book's author name is only a *new* target when no target
        // already covers it under another spelling. Targets used to be keyed by
        // the exact string, so a book crediting "George R.R. Martin" became a
        // second search alongside the registered "George R. R. Martin" — the
        // search that auto-registered a duplicate author for him (SKADI-T-0653).
        let mut covered: HashSet<String> = targets.keys().map(|n| name_key(n)).collect();
        match self.repo.list_books(BookFilter::default()).await {
            Ok(books) => {
                for book in books {
                    for name in book.authors {
                        let name = name.trim();
                        if !name.is_empty() && covered.insert(name_key(name)) {
                            targets.entry(name.to_string()).or_insert(None);
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "catalog ingest: listing books failed"),
        }

        // Name order, so a pass that stops early hands over to the *next* author
        // rather than to whoever a HashMap happens to yield (SKADI-T-0650).
        let mut targets: Vec<(String, Option<AsinId>)> = targets.into_iter().collect();
        targets.sort_by(|a, b| a.0.cmp(&b.0));
        if targets.is_empty() {
            return 0;
        }

        let mut cursor = self.cursor.lock().await;
        let start = cursor
            .next_author
            .as_ref()
            .and_then(|n| targets.iter().position(|(t, _)| t >= n))
            .unwrap_or(0);
        cursor.next_author = None;

        let mut budget = max_pages;
        let mut upserted = 0usize;
        let count = targets.len();
        for step in 0..count {
            let (name, asin) = &targets[(start + step) % count];
            if budget == 0 {
                cursor.next_author = Some(name.clone());
                tracing::info!(
                    pages = max_pages,
                    resume_at = %name,
                    "catalog ingest: per-pass page budget spent; resuming next pass"
                );
                break;
            }
            let from = cursor.resume_page.remove(name).unwrap_or(0);
            let allowance = budget.min(MAX_PAGES_PER_AUTHOR.saturating_sub(from));
            match ingest_author_pages(
                &self.store,
                self.catalog.as_ref(),
                name,
                asin.as_ref(),
                from,
                allowance,
            )
            .await
            {
                Ok(progress) => {
                    budget = budget.saturating_sub(progress.pages_read);
                    upserted += progress.upserted;
                    // Register an author entity for a library-derived author (one
                    // with no registered ASIN yet) so the author-scope browse and
                    // Watch button work for an imported library (SKADI-T-0160).
                    // Know-only: monitored=false, no watcher — acquires nothing
                    // until the operator watches. Idempotent via get_by_asin
                    // (upsert conflicts on id, and Author::new mints a fresh id).
                    // Never auto-register a second author for a name that already
                    // has one: two Audible pages for one person is how
                    // B0DNQBC8G7 arrived (SKADI-T-0653). An operator can still add
                    // an author explicitly; only this automatic path is guarded.
                    if asin.is_none()
                        && let Some(a_asin) = progress.resolved.clone()
                        && matches!(self.repo.get_author_by_asin(&a_asin).await, Ok(None))
                        && !self.name_already_registered(name).await
                    {
                        let mut author = Author::new(name.clone());
                        author.asin = Some(a_asin);
                        author.monitored = false;
                        if let Err(e) = self.repo.upsert_author(&author).await {
                            tracing::warn!(author = %name, error = %e, "catalog ingest: author register failed");
                        }
                    }
                    match progress.next_page {
                        Some(next) if next >= MAX_PAGES_PER_AUTHOR => tracing::warn!(
                            author = %name,
                            pages = MAX_PAGES_PER_AUTHOR,
                            "catalog ingest: page ceiling reached; the rest of this author's catalog was not read"
                        ),
                        Some(next) => {
                            // Cut off by the pass budget, not the ceiling: resume
                            // here, at this page, next pass.
                            cursor.resume_page.insert(name.clone(), next);
                            cursor.next_author = Some(name.clone());
                            break;
                        }
                        None => {}
                    }
                }
                Err(e) => {
                    // At least the failing request was spent.
                    budget = budget.saturating_sub(1);
                    tracing::warn!(author = %name, error = %e, "catalog ingest: failed");
                }
            }
        }
        if upserted > 0 {
            tracing::info!("catalog ingest: upserted {upserted} work(s) this pass");
        }
        upserted
    }

    /// Whether a registered author already carries this name (by name key).
    async fn name_already_registered(&self, name: &str) -> bool {
        let want = name_key(name);
        match self.repo.list_authors(AuthorFilter::default()).await {
            Ok(authors) => authors.iter().any(|a| name_key(&a.name) == want),
            // Unknown → do not register; a missed registration is retried next
            // pass, a duplicate author is not undone by anything.
            Err(_) => true,
        }
    }

    /// Resolve active watchers into acquisitions: for each watcher, find the
    /// known works it covers that aren't already owned, and add them as monitored
    /// `Missing` books (the hunter then acquires). Bounded per pass; best-effort.
    pub async fn resolve_watchers_once(&self) -> usize {
        let Some((profile, root_folder)) = self.resolve_defaults().await else {
            return 0;
        };
        let watchers = match self.store.list_watchers().await {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "watcher resolve: listing watchers failed");
                return 0;
            }
        };
        let mut added = 0usize;
        for w in watchers {
            if added >= MAX_ACQUIRE_PER_PASS {
                tracing::info!("watcher resolve: per-pass cap ({MAX_ACQUIRE_PER_PASS}) hit");
                break;
            }
            let key = AsinId(w.key.clone());
            let works = match w.scope {
                WatchScope::Author => self.store.list_works_by_author(&key).await,
                WatchScope::Series => self.store.list_works_by_series(&key).await,
                WatchScope::Book => self
                    .store
                    .get_work(&key)
                    .await
                    .map(|o| o.into_iter().collect()),
            }
            .unwrap_or_default();

            for work in works {
                if added >= MAX_ACQUIRE_PER_PASS {
                    break;
                }
                // English-only (SKADI-I-0051): never acquire a positively-non-English
                // work, even if an older ingest stored it. This is the gate that
                // stopped the fetcher grabbing German/French/Spanish editions.
                if !crate::work::is_catalog_language(work.language.as_deref()) {
                    continue;
                }
                // Skip works already in the library — by ASIN, and by work identity
                // (SKADI-T-0400). Audible lists Full Cast / Booktrack / re-issued
                // editions as separate products with their own ASINs, so an
                // ASIN-only check let the watcher add a second monitored row for a
                // book already on disk, which the sweep then went and grabbed.
                if matches!(self.repo.get_book_by_asin(&work.asin).await, Ok(Some(_))) {
                    continue;
                }
                // The library already holds this work under another edition. Attach
                // the newly-discovered one to that book rather than creating a
                // second `books` row (SKADI-T-0562) — the duplication that made
                // SKADI-T-0400 necessary.
                //
                // **Unmonitored** (operator decision, 2026-09-09): the edition
                // becomes visible and selectable, and nothing fetches it until
                // someone opts in. Attaching it monitored would reintroduce
                // exactly what T-0400 stopped, only deliberately.
                if let Some(book) = self.library_holds_work(&work).await {
                    let kind = crate::book_file::edition_kind_from_title(&work.title);
                    // An unrecognised title infers `unabridged`, which the book
                    // already has, so this write is refused by
                    // `UNIQUE(book_id, kind_slug)` — correct, because such a work
                    // is a re-listing of the same edition, not a new one.
                    let edition = crate::book_file::BookFile::discovered_of_kind(book.id, kind);
                    match self.repo.upsert_book_file(&edition).await {
                        Ok(()) => tracing::info!(
                            asin = %work.asin.0,
                            title = %work.title,
                            book = %book.id,
                            %kind,
                            "watcher resolve: attached a new edition (unmonitored)"
                        ),
                        Err(e) => tracing::debug!(
                            asin = %work.asin.0,
                            error = %e,
                            "watcher resolve: the book already holds this edition kind"
                        ),
                    }
                    continue;
                }
                match add_book(
                    self.repo.as_ref(),
                    self.enrich.as_ref(),
                    work.asin.clone(),
                    profile,
                    root_folder.clone(),
                )
                .await
                {
                    Ok(book) => {
                        added += 1;
                        tracing::info!(scope = w.scope.as_str(), asin = %work.asin.0, title = %book.title, "watcher resolve: acquiring");
                    }
                    Err(AppError::Validation(m)) if m.contains("already exists") => {}
                    Err(e) => {
                        tracing::warn!(asin = %work.asin.0, error = %e, "watcher resolve: add_book failed")
                    }
                }
            }
        }
        if added > 0 {
            tracing::info!("watcher resolve: queued {added} book(s) for acquisition");
        }
        added
    }

    /// Resolve the default quality profile + root folder for auto-added books,
    /// mirroring the HTTP add path. The profile is the stable built-in audiobook
    /// profile (audiobooks rank via the built-in ladder, not a stored movie
    /// `profiles` row — SKADI-T-0142); the root is derived from the single
    /// `library.root` (SKADI-T-0302), so it is always present.
    async fn resolve_defaults(&self) -> Option<(ProfileId, RootFolder)> {
        let profile = crate::audiobook_builtin_profile_id();
        let root = RootFolder::for_domain(
            crate::http::library_root(&self.store).await,
            skadi_core::MediaKind::Audiobook,
        );
        Some((profile, root))
    }
}

/// Daemon-side worker that runs an author-discovery pass on an interval. Shares
/// nothing with the Cloacina runner (discovery is plain async); on cancel it
/// simply stops looping.
pub struct AuthorDiscoveryWorker {
    discovery: Arc<AuthorDiscovery>,
    interval: Duration,
}

impl AuthorDiscoveryWorker {
    #[must_use]
    pub fn new(discovery: Arc<AuthorDiscovery>, interval: Duration) -> Self {
        Self {
            discovery,
            interval,
        }
    }
}

impl Worker for AuthorDiscoveryWorker {
    fn name(&self) -> &str {
        "audiobooks-author-discovery"
    }

    fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let mut ticker = tokio::time::interval(self.interval);
            ticker.tick().await; // skip the immediate first tick
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => { self.discovery.run_once().await; }
                }
            }
        })
    }
}

impl AuthorDiscovery {
    /// Does the library already hold this work under a different edition?
    ///
    /// Compares the catalog item's work identity (normalized title + primary
    /// author, edition qualifiers stripped) against every book already stored —
    /// monitored or not — that has an imported file. Audible's Full Cast /
    /// Booktrack / re-issue products carry their own ASINs, so the ASIN check
    /// alone let those through (SKADI-T-0400).
    async fn library_holds_work(&self, work: &crate::work::Work) -> Option<crate::book::Book> {
        let Ok(books) = self
            .repo
            .list_books(crate::repo::BookFilter {
                monitored: None,
                limit: None,
                offset: None,
            })
            .await
        else {
            return None;
        };
        let lower = work.title.to_ascii_lowercase();
        let title = crate::wanted::EDITION_SUFFIX_RE.replace_all(&lower, "");
        let author = work.authors.first().map(String::as_str).unwrap_or_default();
        let key = format!(
            "{}|{}",
            crate::wanted::alnum_key(&title),
            crate::wanted::alnum_key(author)
        );
        books
            .into_iter()
            .filter(|b| {
                b.files
                    .iter()
                    .any(|f| matches!(f.status, skadi_core::AcquisitionStatus::Imported { .. }))
            })
            .find(|b| crate::wanted::work_key(b) == key)
    }
}

#[cfg(test)]
mod discover_tests {
    use super::*;

    fn work(asin: &str, date: Option<&str>) -> Work {
        let mut w = Work::new(AsinId(asin.into()), asin);
        w.release_date = date.map(str::to_string);
        w
    }

    // --- SKADI-T-0653: attribution comes from the product's own credits ---

    fn item(credits: &[(&str, Option<&str>)]) -> CatalogItem {
        CatalogItem {
            asin: AsinId("P1".into()),
            title: "A Book".into(),
            release_date: None,
            authors: credits.iter().map(|(n, _)| (*n).to_string()).collect(),
            author_asins: credits
                .iter()
                .map(|(_, a)| a.map(|x| AsinId(x.into())))
                .collect(),
            author_asin: credits
                .first()
                .and_then(|(_, a)| a.map(|x| AsinId(x.into()))),
            cover_url: None,
            series_asin: None,
            series_name: None,
            series_position: None,
            language: Some("english".into()),
        }
    }

    const GRRM: &str = "B000APIGH4";

    fn grrm() -> AsinId {
        AsinId(GRRM.into())
    }

    /// SKADI-T-0656: whatever discovery decides for a product, the startup
    /// repair must decide the same for the work stored from it — keep what was
    /// credited, clear what was not. They disagreed on 85 works when the repair
    /// judged by name and discovery by ASIN, and each undid the other.
    #[test]
    fn discovery_and_the_link_repair_agree() {
        let author = "Derek Kunsken";
        let key = name_key(author);
        let fixtures: Vec<CatalogItem> = vec![
            item(&[("Derek Kunsken", Some(GRRM))]),
            item(&[("Derek Künsken", Some(GRRM))]),
            item(&[("Kunsken Derek", Some(GRRM))]),
            item(&[("Derek Kunsken", None)]),
            item(&[("Derek Kunsken", Some("OTHER"))]),
            item(&[("Someone Else", Some("OTHER"))]),
            item(&[("Someone Else", None)]),
            item(&[("Someone Else", None), ("Derek Künsken", Some(GRRM))]),
            item(&[]),
        ];
        for i in &fixtures {
            let decided = credited_asin(i, &key, Some(&grrm()));
            // Store the work as if attributed to the author, which is what a
            // stale or wrong attribution looks like to the repair.
            let mut w = work_from_catalog(i, Some(grrm()));
            w.author_asin = Some(grrm());
            assert_eq!(
                crate::roles::keeps_attribution(&w, &key),
                decided.is_some(),
                "disagree on {:?} / {:?}",
                i.authors,
                i.author_asins
            );
        }
    }

    #[test]
    fn a_product_crediting_the_registered_asin_is_theirs() {
        let i = item(&[("George R. R. Martin", Some(GRRM))]);
        assert_eq!(
            credited_asin(&i, &name_key("George R. R. Martin"), Some(&grrm())),
            Some(Some(grrm()))
        );
    }

    /// The production case: Audible's fuzzy search for "George R. R. Martin"
    /// returns George R. Martin III's books under his own ASIN.
    #[test]
    fn a_different_person_returned_by_the_fuzzy_search_is_not_theirs() {
        let i = item(&[("George R. Martin III", Some("B00PUVY1AE"))]);
        assert_eq!(
            credited_asin(&i, &name_key("George R. R. Martin"), Some(&grrm())),
            None
        );
    }

    #[test]
    fn a_matching_name_without_an_asin_still_counts() {
        // Translated and older editions often drop the contributor ASIN.
        let i = item(&[("George R.R. Martin", None)]);
        assert_eq!(
            credited_asin(&i, &name_key("George R. R. Martin"), Some(&grrm())),
            Some(Some(grrm()))
        );
    }

    #[test]
    fn a_matching_name_under_a_different_asin_is_somebody_else() {
        let i = item(&[("George R. R. Martin", Some("B0OTHERPAGE"))]);
        assert_eq!(
            credited_asin(&i, &name_key("George R. R. Martin"), Some(&grrm())),
            None,
            "the ASIN is the identity; a matching name is only a hint"
        );
    }

    #[test]
    fn a_co_author_credit_counts() {
        let i = item(&[
            ("Gardner Dozois", None),
            ("George R. R. Martin", Some(GRRM)),
        ]);
        assert_eq!(
            credited_asin(&i, &name_key("George R. R. Martin"), Some(&grrm())),
            Some(Some(grrm()))
        );
    }

    #[test]
    fn a_product_crediting_nobody_keeps_the_searched_author() {
        let i = item(&[]);
        assert_eq!(
            credited_asin(&i, &name_key("Andy Weir"), Some(&grrm())),
            Some(Some(grrm()))
        );
        assert_eq!(credited_asin(&i, &name_key("Andy Weir"), None), Some(None));
    }

    #[test]
    fn an_unregistered_target_takes_the_matching_contributors_asin() {
        let i = item(&[
            ("Gardner Dozois", Some("GD")),
            ("George R. R. Martin", Some(GRRM)),
        ]);
        assert_eq!(
            credited_asin(&i, &name_key("George R. R. Martin"), None),
            Some(Some(grrm()))
        );
        let other = item(&[("George R. Martin III", Some("B00PUVY1AE"))]);
        assert_eq!(
            credited_asin(&other, &name_key("George R. R. Martin"), None),
            None
        );
    }

    #[test]
    fn name_key_normalizes_punctuation_and_spacing() {
        // The library spells it one way, Audible another — both must key equal so
        // the author resolves (SKADI-T-0363).
        assert_eq!(name_key("A.G. Riddle"), name_key("A. G. Riddle"));
        assert_eq!(
            name_key("Brandon Sanderson"),
            name_key("brandon  sanderson")
        );
        // Changed deliberately in SKADI-T-0652. This used to assert the two keyed
        // DIFFERENTLY — which enshrined the bug: "Jim Butcher - editor" is Jim
        // Butcher, credited as editor, and the split made him two authors. The
        // role is a credit, not part of the name.
        assert_eq!(name_key("Jim Butcher"), name_key("Jim Butcher - editor"));
        // But genuinely different names stay distinct (no over-eager fuzzy match),
        // and an unknown suffix is not treated as a role.
        assert_ne!(name_key("Ann Leckie"), name_key("Anne Leckie"));
        assert_ne!(name_key("Prince"), name_key("Prince - Remastered"));
    }

    #[test]
    fn rank_discoveries_orders_upcoming_then_recent_and_drops_owned() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();
        let works = vec![
            work("OWNED", Some("2025-01-01")),
            work("UP2", Some("2026-12-01")),
            work("UP1", Some("2026-07-01")),
            work("OLD", Some("2020-01-01")),
            work("NEW", Some("2026-01-01")),
            work("NONE", None),
        ];
        let owned: HashSet<String> = std::iter::once("OWNED".to_string()).collect();
        let ranked = rank_discoveries(works, &owned, today);
        let order: Vec<&str> = ranked.iter().map(|w| w.asin.0.as_str()).collect();
        // upcoming soonest→latest, then released newest→oldest, then undated; owned dropped.
        assert_eq!(order, vec!["UP1", "UP2", "NEW", "OLD", "NONE"]);
    }
}
