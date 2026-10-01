//! One-off normalisation of contributor roles already stored (SKADI-T-0652).
//!
//! Ingest now parses " - editor", " - translator" and the rest out of author
//! strings (`skadi_metadata::contributors`). Data stored before that still holds
//! them: books and known works whose `authors` include role-suffixed strings, and
//! author *records* registered under names like "Gardner Dozois - editor".
//!
//! [`normalize_contributor_roles`] rewrites those, and is **idempotent** — run it
//! twice and the second run changes nothing — so it is safe to run at every
//! startup ([`RoleNormalizerWorker`]) rather than tracking whether it has run.

use std::collections::HashMap;

use skadi_core::Result;
use skadi_metadata::{select_author_names, select_authors, split_role};

use crate::author::Author;
use crate::repo::{AudiobooksRepo, AuthorFilter, BookFilter, WorksRepo};

/// What one normalisation run changed. All zero on a second run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NormalizeReport {
    /// Books whose author list was rewritten.
    pub books: usize,
    /// Known works whose author list was rewritten.
    pub works: usize,
    /// Author records renamed in place.
    pub authors_renamed: usize,
    /// Author records folded into an existing record for the same person.
    pub authors_merged: usize,
    /// Books left alone because parsing would have left them with no author at
    /// all (e.g. only a translator credited). A background job should not
    /// destroy information; the next metadata refresh re-derives them from
    /// upstream with the ingest rule.
    pub books_skipped_empty: usize,
}

impl NormalizeReport {
    #[must_use]
    pub fn changed_anything(&self) -> bool {
        self.books + self.works + self.authors_renamed + self.authors_merged > 0
    }
}

/// Rewrite stored author strings and author records with the ingest rule.
///
/// Author records: a role-suffixed record is renamed to the bare name. If a
/// record for that bare name already exists, the two are **merged only when
/// they cannot be two different people** — same ASIN, or one of them has none —
/// keeping the one with the ASIN and re-pointing its books. Two records with
/// *different* ASINs are renamed but left separate: deciding whether two Audible
/// identities are one author is SKADI-T-0653's job, not something to settle by
/// string comparison here.
pub async fn normalize_contributor_roles<S>(store: &S) -> Result<NormalizeReport>
where
    S: AudiobooksRepo + WorksRepo,
{
    let mut report = NormalizeReport::default();

    for mut book in store.list_books(BookFilter::default()).await? {
        let clean = select_author_names(book.authors.clone());
        if clean == book.authors {
            continue;
        }
        if clean.is_empty() {
            report.books_skipped_empty += 1;
            tracing::info!(book = %book.title, authors = ?book.authors,
                "role normalisation: left alone — no plain or editorial author");
            continue;
        }
        tracing::info!(book = %book.title, from = ?book.authors, to = ?clean,
            "role normalisation: book authors");
        book.authors = clean;
        store.upsert_book(&book).await?;
        report.books += 1;
    }

    let mut changed_works = Vec::new();
    for mut work in store.list_all_works().await? {
        // Names and ASINs are filtered together so they stay aligned
        // (SKADI-T-0656). A work stored without ASINs keeps none.
        let had_asins = !work.author_asins.is_empty();
        let pairs = work.authors.iter().cloned().zip(
            work.author_asins
                .iter()
                .cloned()
                .chain(std::iter::repeat(None)),
        );
        let (clean, asins): (Vec<String>, Vec<_>) = select_authors(pairs).into_iter().unzip();
        if clean == work.authors || clean.is_empty() {
            continue;
        }
        work.authors = clean;
        work.author_asins = if had_asins { asins } else { Vec::new() };
        changed_works.push(work);
    }
    if !changed_works.is_empty() {
        report.works = changed_works.len();
        store.upsert_works(&changed_works).await?;
        tracing::info!(count = report.works, "role normalisation: known works");
    }

    let authors = store.list_authors(AuthorFilter::default()).await?;
    let by_name: HashMap<String, Author> = authors
        .iter()
        .filter(|a| split_role(&a.name).1.is_none())
        .map(|a| (a.name.trim().to_lowercase(), a.clone()))
        .collect();
    for author in authors {
        let (clean, role) = split_role(&author.name);
        if role.is_none() {
            continue;
        }
        match by_name.get(&clean.to_lowercase()) {
            Some(existing) if same_person(existing, &author) => {
                // Keep whichever has an ASIN; with equal ASINs, keep the one
                // already named correctly.
                let (keep, drop) = if existing.asin.is_some() || author.asin.is_none() {
                    (existing.clone(), author)
                } else {
                    (
                        Author {
                            name: clean.clone(),
                            ..author.clone()
                        },
                        existing.clone(),
                    )
                };
                // Order matters: re-point books while both rows exist, delete the
                // dropped row, and only then rename the kept one — renaming first
                // could collide with the name the dropped row still holds.
                for mut b in store.list_books_by_author(drop.id).await? {
                    b.author_id = Some(keep.id);
                    store.upsert_book(&b).await?;
                }
                store.delete_author(drop.id).await?;
                if keep.id != existing.id {
                    store.upsert_author(&keep).await?;
                }
                tracing::info!(from = %drop.name, into = %keep.name,
                    "role normalisation: merged author records");
                report.authors_merged += 1;
            }
            _ => {
                tracing::info!(from = %author.name, to = %clean,
                    "role normalisation: renamed author record");
                let renamed = Author {
                    name: clean,
                    ..author
                };
                store.upsert_author(&renamed).await?;
                report.authors_renamed += 1;
            }
        }
    }

    Ok(report)
}

/// What [`repair_author_links`] changed. All zero on a second run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LinkReport {
    /// Known works whose `author_asin` pointed at a registered author the work
    /// does not credit, now cleared.
    pub works_unattributed: usize,
    /// Library books linked to their author record.
    pub books_linked: usize,
    /// Books left unlinked because their author's name matches more than one
    /// registered author.
    pub books_ambiguous: usize,
}

/// Repair author links in stored data (SKADI-T-0653). Idempotent.
///
/// 1. **Mis-attributed works.** Discovery used to credit every product a fuzzy
///    `author=` search returned to the searched author, so known works carry an
///    `author_asin` for an author they do not name — George R. Martin III's books
///    under George R. R. Martin. A work is judged with discovery's own rule
///    ([`crate::discovery::credits`]) against the registered author holding its
///    `author_asin`, and the link is cleared when that rule does not credit it.
///
///    Only a work that **stores its contributor ASINs** is judged. Judging the
///    others by name alone disagreed with discovery, which judges by ASIN:
///    "Derek Künsken" credited under Derek Kunsken's ASIN, "Fritz Leiber Jr.",
///    "Hammett Dashiell" — 85 works on production were cleared here at every
///    start and restored by discovery hours later (SKADI-T-0656). A work stored
///    before ASINs were kept is left alone until discovery rewrites it. A work
///    pointing at an ASIN with no registered author is also left alone: nothing
///    to judge it against.
/// 2. **Unlinked books.** Library books are linked to an author record by name
///    key, but only when **exactly one** registered author matches. Two matches
///    — e.g. two Audible pages for one person — leaves the book unlinked rather
///    than guessing; clients then group it by name, which is the right row.
pub async fn repair_author_links<S>(store: &S) -> Result<LinkReport>
where
    S: AudiobooksRepo + WorksRepo,
{
    use crate::discovery::name_key;
    let mut report = LinkReport::default();
    let authors = store.list_authors(AuthorFilter::default()).await?;

    let key_by_asin: HashMap<String, String> = authors
        .iter()
        .filter_map(|a| a.asin.as_ref().map(|x| (x.0.clone(), name_key(&a.name))))
        .collect();
    for w in store.list_all_works().await? {
        let Some(asin) = w.author_asin.as_ref() else {
            continue;
        };
        let Some(key) = key_by_asin.get(&asin.0) else {
            continue;
        };
        if !keeps_attribution(&w, key) {
            tracing::info!(work = %w.title, credited = ?w.authors, was = %asin.0,
                "author links: cleared a work's attribution to an author it does not credit");
            // Not `upsert_works`: its changeset skips `None`, so it cannot clear.
            store.clear_work_author(&w.asin).await?;
            report.works_unattributed += 1;
        }
    }

    let mut by_key: HashMap<String, Vec<&Author>> = HashMap::new();
    for a in &authors {
        by_key.entry(name_key(&a.name)).or_default().push(a);
    }
    for mut book in store.list_books(BookFilter::default()).await? {
        if book.author_id.is_some() {
            continue;
        }
        let Some(first) = book.authors.first() else {
            continue;
        };
        match by_key.get(&name_key(first)).map(Vec::as_slice) {
            Some([only]) => {
                book.author_id = Some(only.id);
                store.upsert_book(&book).await?;
                report.books_linked += 1;
            }
            Some(many) if many.len() > 1 => report.books_ambiguous += 1,
            _ => {}
        }
    }
    Ok(report)
}

/// Whether a stored work keeps its `author_asin`, given the name key of the
/// registered author holding that ASIN. True when there is no ASIN evidence to
/// judge by; otherwise exactly what discovery decides for the same contributors.
pub(crate) fn keeps_attribution(w: &crate::work::Work, author_key: &str) -> bool {
    if w.author_asins.is_empty() {
        return true;
    }
    crate::discovery::credits(
        &w.authors,
        &w.author_asins,
        author_key,
        w.author_asin.as_ref(),
    )
    .is_some()
}

/// Two records may be one person only when nothing says otherwise: matching
/// ASINs, or at least one record without an ASIN.
fn same_person(a: &Author, b: &Author) -> bool {
    match (&a.asin, &b.asin) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// Startup maintenance of author data: runs [`normalize_contributor_roles`]
/// (SKADI-T-0652) and then [`repair_author_links`] (SKADI-T-0653) once when the
/// audiobooks domain starts, then exits. Both are idempotent, so running at
/// every start is cheaper than tracking whether they already ran. Roles first,
/// so link repair compares clean names.
pub struct AuthorMaintenanceWorker<S> {
    store: S,
}

impl<S> AuthorMaintenanceWorker<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S> skadi_core::Worker for AuthorMaintenanceWorker<S>
where
    S: AudiobooksRepo + WorksRepo + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "audiobooks-author-maintenance"
    }

    fn run(
        self: Box<Self>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> skadi_core::module::BoxFuture<'static, ()> {
        Box::pin(async move {
            match normalize_contributor_roles(&self.store).await {
                Ok(r) if r.changed_anything() => {
                    tracing::info!(report = ?r, "role normalisation: complete");
                }
                Ok(r) => tracing::debug!(report = ?r, "role normalisation: nothing to do"),
                Err(e) => tracing::warn!(error = %e, "role normalisation: failed"),
            }
            match repair_author_links(&self.store).await {
                Ok(r) if r != LinkReport::default() => {
                    tracing::info!(report = ?r, "author links: repaired");
                }
                Ok(_) => tracing::debug!("author links: nothing to repair"),
                Err(e) => tracing::warn!(error = %e, "author links: repair failed"),
            }
        })
    }
}
