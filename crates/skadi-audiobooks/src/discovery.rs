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
            let mut w = work_from_catalog(item, author_asin);
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

        // Resolve the author's own ASIN: prefer the caller's, else the catalog's for
        // a product whose primary author (authors[0]) is the queried name AND that
        // carries an ASIN — the SAME author appears both with and without an ASIN
        // across a catalog (e.g. a translated edition drops it), and only some items
        // include it, so we must keep scanning past the ASIN-less matches. Name match
        // is normalized (case/'.'/spacing-insensitive) so "A.G. Riddle" ==
        // "A. G. Riddle". Kept across pages: first match wins.
        if progress.resolved.is_none() {
            progress.resolved = page
                .items
                .iter()
                .find(|it| {
                    it.author_asin.is_some()
                        && it.authors.first().is_some_and(|a| name_key(a) == want)
                })
                .and_then(|it| it.author_asin.clone());
        }

        if last {
            return Ok(progress);
        }
        index += 1;
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
fn name_key(name: &str) -> String {
    let (name, _) = skadi_metadata::split_role(name);
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Build a [`Work`] from an Audible [`CatalogItem`] + the owning author's ASIN.
fn work_from_catalog(item: &CatalogItem, author_asin: Option<&AsinId>) -> Work {
    let mut w = Work::new(item.asin.clone(), item.title.clone());
    w.authors = item.authors.clone();
    // Prefer the registered author's ASIN; otherwise adopt the one the catalog
    // carries so a library-derived author's works are still queryable by author.
    w.author_asin = author_asin.cloned().or_else(|| item.author_asin.clone());
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

        match self.repo.list_books(BookFilter::default()).await {
            Ok(books) => {
                for book in books {
                    for name in book.authors {
                        let name = name.trim();
                        if !name.is_empty() {
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
                    if asin.is_none()
                        && let Some(a_asin) = progress.resolved.clone()
                        && matches!(self.repo.get_author_by_asin(&a_asin).await, Ok(None))
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
