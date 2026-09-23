//! Title relevance scoring (SKADI-T-0180).
//!
//! When a book's title equals its series name (e.g. *Dungeon Crawler Carl* book 1),
//! a title search returns **every** book in the series, all at the same "Unknown"
//! quality, and the hunter's seeders tiebreak would grab whichever sibling is most
//! seeded — the wrong book. [`title_relevance`] gives `decide` a cheap signal to
//! prefer the release that actually matches the requested title: the wrong book
//! carries extra title words (e.g. *Butchers Masquerade*) that the right one
//! doesn't, which lowers its `precision`.

use std::collections::HashSet;

/// How well a release title matches the requested title alias(es).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleRelevance {
    /// Fraction of the **primary** requested title's significant tokens present in
    /// the release (0..=1). Low ⇒ the wanted title isn't really there (unrelated
    /// match on a stray token) — a gate, not a ranking signal.
    pub coverage: f32,
    /// Fraction of the release's significant tokens explained by **any** requested
    /// alias (primary title + the author-appended alias) (0..=1). Low ⇒ the release
    /// carries a lot of *other* content, e.g. a different book's title in the same
    /// series — the ranking signal that separates the right book from its siblings.
    pub precision: f32,
    /// Count of significant tokens in the **primary** requested title. Full
    /// coverage of a one-token title ("Talisman", "Fallen") is weak evidence —
    /// anything that says the word covers it — so callers gate such titles on
    /// something else as well (SKADI-T-0587).
    pub primary_tokens: usize,
    /// Count of significant **author** tokens known for the wanted item — the alias
    /// tokens beyond the primary title (0 ⇒ author unknown, so it can't be gated on).
    pub author_tokens: usize,
    /// How many of those author tokens appear in the release title. The identity gate
    /// (SKADI-T-0359): a real audiobook release names its author, so junk that only
    /// shares the title word ("Fallen" → "Evanescence – Fallen") has `author_hits == 0`
    /// while `author_tokens > 0` — reject it instead of grabbing the wrong media.
    pub author_hits: usize,
}

/// Structural / stop words ignored when tokenizing a title. (Format and genre
/// noise — `audiobook`, `fiction`, narrator names — is deliberately NOT stripped:
/// it appears in *every* release of a series, so it cancels out in the relative
/// `precision` ranking and only needs to not collide with a real title token.)
const STOPWORDS: &[&str] = &["the", "a", "an", "of", "and", "by", "to", "in", "for"];

/// Significant tokens of a title: lowercased alphanumeric runs, dropping pure
/// numbers, 1-char tokens, and stop words.
fn tokens(s: &str) -> HashSet<String> {
    s.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| {
            t.len() >= 2 && !t.bytes().all(|b| b.is_ascii_digit()) && !STOPWORDS.contains(t)
        })
        .map(str::to_string)
        .collect()
}

/// The initialism of a title, when it is long enough to be distinctive
/// (SKADI-T-0181).
///
/// "Dungeon Crawler Carl" → `dcc`. A book released only under an abbreviation
/// tokenises to `{dcc, book}`, shares nothing with `{dungeon, crawler, carl}`,
/// scores coverage 0 and is dropped — unacquirable when no full-title release
/// exists.
///
/// **Three or more significant tokens required.** A two-letter initialism is far
/// too collision-prone to spend the coverage gate on — `dc`, `lo`, `it` appear in
/// release names constantly — and the gate's whole job is rejecting unrelated
/// matches. Word order is preserved because an initialism is ordered, so this
/// takes the tokens in the title's own order rather than the token *set*.
fn initialism(title: &str) -> Option<String> {
    let ordered: Vec<String> = title
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| {
            t.len() >= 2 && !t.bytes().all(|b| b.is_ascii_digit()) && !STOPWORDS.contains(t)
        })
        .map(str::to_string)
        .collect();
    (ordered.len() >= 3).then(|| {
        ordered
            .iter()
            .filter_map(|t| t.chars().next())
            .collect::<String>()
    })
}

/// Score `release_title` against the requested `wanted` title aliases (primary
/// first; a later alias may append the author). Pure.
#[must_use]
pub fn title_relevance(wanted: &[String], release_title: &str) -> TitleRelevance {
    let rel = tokens(release_title);
    let primary: HashSet<String> = wanted.first().map(|t| tokens(t)).unwrap_or_default();
    let want_all: HashSet<String> = wanted.iter().flat_map(|t| tokens(t)).collect();
    // Author (and any non-primary-alias) tokens: everything the aliases carry beyond
    // the primary title. For audiobook seeds the second alias is "<title> <author>",
    // so this is the author's significant tokens.
    let author: HashSet<String> = want_all.difference(&primary).cloned().collect();

    let mut coverage = if primary.is_empty() {
        0.0
    } else {
        primary.intersection(&rel).count() as f32 / primary.len() as f32
    };
    // An initialism stands in for the whole title (SKADI-T-0181): "DCC Book 1"
    // covers "Dungeon Crawler Carl". Only *raises* coverage — a release that
    // already names the title fully must not be dragged down — and the strict
    // token gate still applies through `precision`, so a release whose only
    // relevant token is the acronym still ranks below a full-title one.
    if let Some(acronym) = wanted.first().and_then(|t| initialism(t))
        && rel.contains(&acronym)
    {
        coverage = coverage.max(1.0);
    }
    let precision = if rel.is_empty() {
        0.0
    } else {
        want_all.intersection(&rel).count() as f32 / rel.len() as f32
    };
    TitleRelevance {
        coverage,
        precision,
        primary_tokens: primary.len(),
        author_tokens: author.len(),
        author_hits: author.intersection(&rel).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted() -> Vec<String> {
        vec![
            "Dungeon Crawler Carl".to_string(),
            "Dungeon Crawler Carl Matt Dinniman".to_string(),
        ]
    }

    #[test]
    fn right_book_outranks_wrong_book_in_the_same_series() {
        let w = wanted();
        let book1 = title_relevance(
            &w,
            "Dungeon Crawler Carl (Dungeon Crawler Carl 01) by Matt Dinniman (Audiobook)(Fiction)",
        );
        let book5 = title_relevance(
            &w,
            "The Butchers Masquerade (Dungeon Crawler Carl 05) by Matt Dinniman (Audiobook)(Fiction)",
        );
        // Both have the series name present (so neither is "unrelated")...
        assert!(book1.coverage >= 0.9 && book5.coverage >= 0.9);
        // ...but the right book matches more precisely (no foreign title words).
        assert!(
            book1.precision > book5.precision,
            "book1 {} should beat book5 {}",
            book1.precision,
            book5.precision
        );
        // And they bucket apart (the decide tiebreak granularity).
        assert!((book1.precision * 4.0).round() > (book5.precision * 4.0).round());
    }

    #[test]
    fn unrelated_release_has_low_coverage() {
        let w = wanted();
        // Matches only on the stray token "carl".
        let r = title_relevance(&w, "Carl Sagan - Cosmos (Documentary)");
        assert!(r.coverage < 0.5, "coverage {} should be low", r.coverage);
    }

    #[test]
    fn exact_title_is_full_coverage() {
        let r = title_relevance(
            &["Project Hail Mary".to_string()],
            "Project Hail Mary [M4B]",
        );
        assert_eq!(r.coverage, 1.0);
    }

    // --- identity gate: author presence separates junk from a real match (T-0359) ---

    #[test]
    fn coincidental_title_junk_has_no_author_hit() {
        // Want the audiobook "Fallen" by "Karen Chance"; a music album shares only
        // the title word. coverage is trivially 1.0 (short title) — the OLD gate
        // passed it. The author gate is what rejects it.
        let want = vec!["Fallen".to_string(), "Fallen Karen Chance".to_string()];
        let junk = title_relevance(&want, "Evanescence - Fallen [2003] [Deluxe Edition]");
        assert_eq!(junk.coverage, 1.0, "short title is trivially covered");
        assert!(junk.author_tokens > 0, "we know the author (karen, chance)");
        assert_eq!(junk.author_hits, 0, "no author token appears → reject");

        let real = title_relevance(&want, "Fallen - Karen Chance - Unabridged (M4B)");
        assert!(real.author_hits >= 1, "the real release names the author");
    }

    #[test]
    fn game_junk_for_short_title_has_no_author_hit() {
        // Want "Wasteland" by "W. Scott Poole"; a PC game grabs the title word.
        let want = vec![
            "Wasteland".to_string(),
            "Wasteland W Scott Poole".to_string(),
        ];
        let junk = title_relevance(&want, "Wasteland 3-HOODLUM");
        assert_eq!(junk.coverage, 1.0);
        assert!(junk.author_tokens > 0);
        assert_eq!(junk.author_hits, 0, "HOODLUM is not the author → reject");
    }

    #[test]
    fn author_tokens_zero_when_author_unknown() {
        // No author alias ⇒ author can't be gated on (author_tokens == 0).
        let r = title_relevance(&["Wasteland".to_string()], "Wasteland 3-HOODLUM");
        assert_eq!(r.author_tokens, 0);
    }
}

#[cfg(test)]
mod initialism_tests {
    use super::*;

    /// SKADI-T-0181: a book released only under an abbreviation was unacquirable —
    /// "DCC Book 1" shares no token with "Dungeon Crawler Carl", so coverage was 0
    /// and the gate dropped it.
    #[test]
    fn an_initialism_covers_the_full_title() {
        let wanted = vec!["Dungeon Crawler Carl".to_string()];
        let r = title_relevance(&wanted, "DCC Book 1 2024 M4B");
        assert_eq!(r.coverage, 1.0, "the acronym stands in for the title");

        // The full title still scores full coverage — the acronym only ever raises.
        let full = title_relevance(&wanted, "Dungeon Crawler Carl 2024 M4B");
        assert_eq!(full.coverage, 1.0);
        // …and outranks the abbreviated one on precision, which is what decides
        // between them. The strict token gate is not weakened, only supplemented.
        assert!(
            full.precision > r.precision,
            "full title {} should beat acronym {}",
            full.precision,
            r.precision
        );
    }

    /// Two-token titles get **no** initialism: `dc`, `lo`, `it` appear in release
    /// names constantly, and spending the coverage gate on them would readmit
    /// exactly the unrelated matches it exists to reject.
    #[test]
    fn short_titles_get_no_initialism() {
        assert_eq!(initialism("Dungeon Crawler Carl").as_deref(), Some("dcc"));
        assert_eq!(initialism("Project Hail Mary").as_deref(), Some("phm"));
        assert_eq!(initialism("Dark Matter"), None, "two tokens is too short");
        assert_eq!(initialism("Heat"), None);

        // A stray "dc" must not cover "Dark Matter".
        let wanted = vec!["Dark Matter".to_string()];
        assert_eq!(title_relevance(&wanted, "DC Comics 2024").coverage, 0.0);
    }

    /// Stop words are dropped before the initials are taken, so the acronym is
    /// the one a human would write.
    #[test]
    fn stop_words_do_not_contribute_initials() {
        // "The Lord of the Rings" leaves {lord, rings} once stop words go — two
        // tokens, so no acronym. The stop words are dropped *before* the length
        // check, which is what makes this None rather than "tlotr".
        assert_eq!(initialism("The Lord of the Rings"), None);
        assert_eq!(
            initialism("A Court of Thorns and Roses").as_deref(),
            Some("ctr")
        );
    }
}
