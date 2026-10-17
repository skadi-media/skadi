//! Title ordering (SKADI-T-0648 F3): the one rule for how a title sorts and
//! which A–Z letter it files under, shared by every title surface — the
//! movie, TV and audiobook walls, the series and author clusters, the wanted
//! list. Android has the same rule in `core` `TitleSorting`; the two test the
//! same vector, so the same library reads in the same order on both.
//!
//! Titles drop one leading article ("the ", "a ", "an "), so *The Hobbit*
//! files under H. A person's name does not: "An Na" is a name, and "An" is
//! part of it. Names go through [`crate::audiobooks::name_sort_key`].
//!
//! [`title_letter`] is defined from [`title_sort_key`], and [`railed_by_title`]
//! hands back the sorted items with their letters, so a surface cannot sort
//! by one rule and label by another — which is how the Android authors list
//! went wrong (SKADI-T-0648 F2).

const TITLE_ARTICLES: &[&str] = &["the ", "a ", "an "];

/// Sort key for a title: trimmed, lowercase, one leading article dropped.
#[must_use]
pub fn title_sort_key(title: &str) -> String {
    let t = title.trim().to_lowercase();
    TITLE_ARTICLES
        .iter()
        .find_map(|a| t.strip_prefix(a))
        .map_or_else(|| t.clone(), str::to_string)
}

/// The A–Z letter a title files under, derived from its sort key. A key that
/// does not start with a latin letter (digits, symbols, other scripts) files
/// under `#`, one bucket rather than many.
#[must_use]
pub fn title_letter(title: &str) -> char {
    letter_of_key(&title_sort_key(title))
}

fn letter_of_key(key: &str) -> char {
    match key.chars().next().map(|c| c.to_ascii_uppercase()) {
        Some(c) if c.is_ascii_uppercase() => c,
        _ => '#',
    }
}

/// Sort `items` in place by the title order.
pub fn sort_by_title<T>(items: &mut [T], title: impl Fn(&T) -> &str) {
    items.sort_by_cached_key(|i| title_sort_key(title(i)));
}

/// A list in title order with the rail letter of each item, index for index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Railed<T> {
    pub items: Vec<T>,
    pub letters: Vec<char>,
}

/// Sort `items` by title and pair each with its rail letter. The letters come
/// from the same key the order does, so they can never disagree.
#[must_use]
pub fn railed_by_title<T>(mut items: Vec<T>, title: impl Fn(&T) -> &str) -> Railed<T> {
    sort_by_title(&mut items, &title);
    let letters = items
        .iter()
        .map(|i| letter_of_key(&title_sort_key(title(i))))
        .collect();
    Railed { items, letters }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared title vector: the same titles, keys and letters are asserted
    /// in the Android `TitleSortingTest`, so the two clients provably order
    /// alike. Listed here in the order both must produce.
    const VECTOR: &[(&str, &str, char)] = &[
        ("1984", "1984", '#'),
        (
            "A.I. Artificial Intelligence",
            "a.i. artificial intelligence",
            'A',
        ),
        ("Anathem", "anathem", 'A'),
        ("Dune", "dune", 'D'),
        ("A Game of Thrones", "game of thrones", 'G'),
        ("The Hobbit", "hobbit", 'H'),
        ("  the martian ", "martian", 'M'),
        ("Thelma & Louise", "thelma & louise", 'T'),
        ("Them!", "them!", 'T'),
        ("An Unkindness of Ghosts", "unkindness of ghosts", 'U'),
        ("¡Three Amigos!", "¡three amigos!", '#'),
    ];

    #[test]
    fn shared_title_vector_keys_and_letters() {
        for (title, key, letter) in VECTOR {
            assert_eq!(title_sort_key(title), *key, "{title}");
            assert_eq!(title_letter(title), *letter, "{title}");
        }
    }

    #[test]
    fn shared_title_vector_orders_as_listed() {
        let mut shuffled: Vec<&str> = VECTOR.iter().rev().map(|(t, _, _)| *t).collect();
        shuffled.swap(2, 7);
        let railed = railed_by_title(shuffled, |t| t);
        let want: Vec<&str> = VECTOR.iter().map(|(t, _, _)| *t).collect();
        assert_eq!(railed.items, want);
        let letters: Vec<char> = VECTOR.iter().map(|(_, _, l)| *l).collect();
        assert_eq!(railed.letters, letters);
    }

    #[test]
    fn letter_follows_the_key() {
        for (title, _, _) in VECTOR {
            let from_key = title_sort_key(title)
                .chars()
                .next()
                .map(|c| c.to_ascii_uppercase())
                .filter(char::is_ascii_uppercase)
                .unwrap_or('#');
            assert_eq!(title_letter(title), from_key, "{title}");
        }
        assert_eq!(title_letter(""), '#');
        assert_eq!(title_letter("   "), '#');
        assert_eq!(title_letter("The 100"), '#');
    }

    #[test]
    fn names_keep_their_article() {
        // The title rule and the name rule are different rules on purpose.
        assert_eq!(title_sort_key("An Na"), "na");
        assert_eq!(crate::audiobooks::name_sort_key("An Na"), "na an");
    }
}
