//! Library repair passes for audiobooks.
//!
//! [`merge_book_editions`] folds the duplicate `books` rows that predate the
//! edition model (SKADI-T-0562) into one book with several editions.

use crate::book::Book;
use crate::book_file::BookFile;
use crate::repo::{AudiobooksRepo, BookFilter};
use skadi_core::BookId;
use skadi_core::Result;

/// One group of `books` rows that name the same work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeCandidate {
    /// The row that survives.
    pub canonical: BookId,
    /// Its title, for the operator's report — ids alone are unreviewable.
    pub title: String,
    /// Rows folded into `canonical`.
    pub duplicates: Vec<BookId>,
    /// Editions that move across, as `(edition id, kind)`.
    pub moves: Vec<(String, String)>,
    /// Editions that **cannot** move: the canonical book already holds that
    /// kind, so moving would mean two files of one edition. Reported, never
    /// resolved automatically — see [`merge_book_editions`].
    pub conflicts: Vec<(String, String)>,
}

/// What a merge run did, or in a dry run would do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Books examined.
    pub scanned: usize,
    /// Groups with more than one row.
    pub groups: usize,
    /// Editions re-parented onto a canonical book.
    pub moved: usize,
    /// Duplicate `books` rows deleted.
    pub removed: usize,
    /// Editions left in place because of a kind collision.
    pub conflicted: usize,
    /// The per-group detail, for printing.
    pub candidates: Vec<MergeCandidate>,
}

/// Group the library by work identity and fold duplicates into one book.
///
/// **Identity is `crate::wanted::work_key`** — the same key the ingest guard
/// uses to decide a work is already held. Using a second, merge-only notion of
/// sameness is how a tool like this ends up merging rows that ingest still
/// treats as distinct (or vice versa), so the two either agree by construction
/// or eventually disagree in production.
///
/// **The canonical row is the earliest `added_at`**, ties broken by id so a dry
/// run and the apply that follows it always choose the same row. Earliest is the
/// row an operator most likely created deliberately; the later ones accumulated
/// from watcher discovery.
///
/// **Refs are not rewritten, because they do not point at books.** An audiobook's
/// [`skadi_importer::AcquirableRef`] is the *edition* id
/// ([`BookFile::acquirable_ref`]), so re-parenting an edition — changing its
/// `book_id`, keeping its id — leaves every row in `downloads`,
/// `acquisition_history`, `decision_history`, `blocklist` and `trace_events`
/// pointing at exactly what it pointed at before. The ticket budgeted for a ref
/// rewrite; the edition model had already made it unnecessary. A rewrite pass
/// here would be five table scans that can only introduce errors.
///
/// **A kind collision is reported, never resolved.** If both rows hold an
/// `unabridged` edition, they are two files of the same edition, and picking one
/// means deleting media. That is the operator's call — skadi does not delete
/// files it was not asked to delete. Those editions stay on their original book,
/// which therefore also survives.
///
/// A moved edition keeps its own `monitored` flag: it records what the operator
/// wanted for *that* edition, and the merge is not the moment to reinterpret it.
pub async fn merge_book_editions(repo: &dyn AudiobooksRepo, apply: bool) -> Result<MergeReport> {
    let books = repo
        .list_books(BookFilter {
            monitored: None,
            limit: None,
            offset: None,
        })
        .await?;

    let mut report = MergeReport {
        scanned: books.len(),
        ..MergeReport::default()
    };

    // Group by work identity, preserving first-seen order so the report is
    // stable between runs.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<Book>> = std::collections::HashMap::new();
    for book in books {
        let key = crate::wanted::work_key(&book);
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(book);
    }

    for key in order {
        let mut group = groups.remove(&key).unwrap_or_default();
        if group.len() < 2 {
            continue;
        }
        report.groups += 1;
        group.sort_by(|a, b| {
            a.added_at
                .cmp(&b.added_at)
                // BookId is a uuid newtype without Ord; comparing the string
                // form is only a tiebreak, and it only has to be *stable* so a
                // dry run and the apply that follows pick the same canonical row.
                .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
        });
        let canonical = group.remove(0);

        let mut held: Vec<String> = canonical.files.iter().map(|f| f.kind.clone()).collect();
        let mut cand = MergeCandidate {
            canonical: canonical.id,
            title: canonical.title.clone(),
            duplicates: Vec::new(),
            moves: Vec::new(),
            conflicts: Vec::new(),
        };

        for dup in &group {
            let mut blocked = false;
            for file in &dup.files {
                if held.contains(&file.kind) {
                    cand.conflicts
                        .push((file.id.to_string(), file.kind.clone()));
                    blocked = true;
                    continue;
                }
                held.push(file.kind.clone());
                cand.moves.push((file.id.to_string(), file.kind.clone()));
                if apply {
                    let moved = BookFile {
                        book_id: canonical.id,
                        ..file.clone()
                    };
                    repo.upsert_book_file(&moved).await?;
                }
                report.moved += 1;
            }
            // A book that still owns an edition cannot be deleted: the delete
            // would take the edition — and its file — with it.
            if blocked {
                continue;
            }
            cand.duplicates.push(dup.id);
            if apply {
                repo.delete_book(dup.id).await?;
            }
            report.removed += 1;
        }

        report.conflicted += cand.conflicts.len();
        report.candidates.push(cand);
    }

    Ok(report)
}
