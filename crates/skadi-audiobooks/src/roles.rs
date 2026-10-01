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
use skadi_metadata::{select_author_names, split_role};

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
        let clean = select_author_names(work.authors.clone());
        if clean == work.authors || clean.is_empty() {
            continue;
        }
        work.authors = clean;
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

/// Two records may be one person only when nothing says otherwise: matching
/// ASINs, or at least one record without an ASIN.
fn same_person(a: &Author, b: &Author) -> bool {
    match (&a.asin, &b.asin) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// Runs [`normalize_contributor_roles`] once when the audiobooks domain starts,
/// then exits. Idempotent, so running at every start is cheaper than tracking
/// whether it already ran.
pub struct RoleNormalizerWorker<S> {
    store: S,
}

impl<S> RoleNormalizerWorker<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S> skadi_core::Worker for RoleNormalizerWorker<S>
where
    S: AudiobooksRepo + WorksRepo + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "audiobooks-role-normalizer"
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
        })
    }
}
