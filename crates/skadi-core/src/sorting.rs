//! Sort keys (SKADI-T-0737): the one copy of the title and name order rules.
//!
//! The list endpoints emit these keys on each record (`sort_title`, and
//! `sort_name` for people), so the web and Android clients order by what the
//! server sends instead of each holding a copy of the rules (SKADI-T-0648).
//!
//! A title drops one leading article ("the ", "a ", "an "), so *The Hobbit*
//! files under H. A person's name goes surname first and keeps its words:
//! "An Na" is a name, and "An" is part of it. The rail letter a client shows
//! is the first character of the key, uppercased, or `#` when that is not a
//! latin letter ([`letter_of_key`]).

use serde::Serialize;

const TITLE_ARTICLES: &[&str] = &["the ", "a ", "an "];

const NAME_PARTICLES: &[&str] = &[
    "le", "la", "de", "du", "da", "di", "del", "della", "des", "van", "von", "der", "den", "ter",
    "ten", "dos", "das", "st", "st.", "al", "el", "bin", "ibn",
];
const NAME_SUFFIXES: &[&str] = &[
    "jr", "jr.", "sr", "sr.", "ii", "iii", "iv", "phd", "ph.d.", "md", "m.d.",
];
const COLLECTIVE_CREDITS: &[&str] = &[
    "full cast",
    "various",
    "various authors",
    "anonymous",
    "unknown author",
];

/// Sort key for a title: trimmed, lowercase, one leading article dropped.
#[must_use]
pub fn title_sort_key(title: &str) -> String {
    let t = title.trim().to_lowercase();
    TITLE_ARTICLES
        .iter()
        .find_map(|a| t.strip_prefix(a))
        .map_or_else(|| t.clone(), str::to_string)
}

/// Sort key for a person's name, surname first: "Stephen King" orders as
/// "king stephen". The surname is the last word plus any particles before it
/// ("Le Guin", "van Vogt"), ignoring a suffix ("Jr.", "III"). "Surname, Given"
/// is taken as written; collective credits ("Full Cast") keep their order.
#[must_use]
pub fn name_sort_key(name: &str) -> String {
    let n = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if n.is_empty() || COLLECTIVE_CREDITS.contains(&n.as_str()) {
        return n;
    }
    if let Some((head, tail)) = n.split_once(", ")
        && !NAME_SUFFIXES.contains(&tail.trim_end_matches(','))
    {
        return format!("{head} {tail}");
    }
    let mut words: Vec<&str> = n.split(' ').map(|w| w.trim_end_matches(',')).collect();
    while words.len() > 1 && NAME_SUFFIXES.contains(words.last().expect("non-empty")) {
        words.pop();
    }
    if words.len() == 1 {
        return words[0].to_string();
    }
    let mut start = words.len() - 1;
    while start > 1 && NAME_PARTICLES.contains(&words[start - 1]) {
        start -= 1;
    }
    let mut out = words[start..].to_vec();
    out.extend_from_slice(&words[..start]);
    out.join(" ")
}

/// The A–Z letter a key files under. A key that does not start with a latin
/// letter (digits, symbols, other scripts) files under `#`.
#[must_use]
pub fn letter_of_key(key: &str) -> char {
    match key.chars().next().map(|c| c.to_ascii_uppercase()) {
        Some(c) if c.is_ascii_uppercase() => c,
        _ => '#',
    }
}

/// A list record with its title sort key: the record's own fields, plus
/// `sort_title`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Titled<T> {
    #[serde(flatten)]
    pub record: T,
    pub sort_title: String,
}

impl<T> Titled<T> {
    /// Wrap `record`, keyed by `title`.
    #[must_use]
    pub fn new(record: T, title: &str) -> Self {
        Self {
            sort_title: title_sort_key(title),
            record,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared title vector, in the order a list must come out in. The
    /// clients' fallback tests and the Android `TitleSortingTest` read the
    /// same titles.
    const TITLES: &[(&str, &str, char)] = &[
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

    /// The shared name vector, surname first.
    const NAMES: &[(&str, &str)] = &[
        ("Stephen King", "king stephen"),
        ("A. G. Riddle", "riddle a. g."),
        ("Ursula K. Le Guin", "le guin ursula k."),
        ("A. E. van Vogt", "van vogt a. e."),
        ("Fritz Leiber Jr.", "leiber fritz"),
        ("Fritz Leiber, Jr.", "leiber fritz"),
        ("George R. Martin III", "martin george r."),
        ("Hammett, Dashiell", "hammett dashiell"),
        ("Van Morrison", "morrison van"),
        ("bell hooks", "hooks bell"),
        ("Plato", "plato"),
        ("Full Cast", "full cast"),
        ("  Alan   Lee ", "lee alan"),
        ("An Na", "na an"),
    ];

    #[test]
    fn shared_title_vector_keys_and_letters() {
        for (title, key, letter) in TITLES {
            assert_eq!(title_sort_key(title), *key, "{title}");
            assert_eq!(letter_of_key(&title_sort_key(title)), *letter, "{title}");
        }
    }

    #[test]
    fn shared_title_vector_orders_as_listed() {
        let mut shuffled: Vec<&str> = TITLES.iter().rev().map(|(t, _, _)| *t).collect();
        shuffled.swap(2, 7);
        shuffled.sort_by_cached_key(|t| title_sort_key(t));
        let want: Vec<&str> = TITLES.iter().map(|(t, _, _)| *t).collect();
        assert_eq!(shuffled, want);
    }

    #[test]
    fn edge_titles() {
        assert_eq!(letter_of_key(&title_sort_key("")), '#');
        assert_eq!(letter_of_key(&title_sort_key("   ")), '#');
        assert_eq!(letter_of_key(&title_sort_key("The 100")), '#');
        assert_eq!(title_sort_key("Theodore"), "theodore");
        assert_eq!(title_sort_key("Anora"), "anora");
        assert_eq!(title_sort_key("The The"), "the");
    }

    #[test]
    fn shared_name_vector() {
        for (name, key) in NAMES {
            assert_eq!(name_sort_key(name), *key, "{name}");
        }
    }

    #[test]
    fn names_keep_their_article() {
        // The title rule and the name rule are different rules on purpose.
        assert_eq!(title_sort_key("An Na"), "na");
        assert_eq!(name_sort_key("An Na"), "na an");
    }

    #[test]
    fn a_titled_record_flattens_and_adds_its_key() {
        #[derive(Serialize)]
        struct M {
            id: u8,
            title: String,
        }
        let t = Titled::new(
            M {
                id: 1,
                title: "The Hobbit".into(),
            },
            "The Hobbit",
        );
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"id": 1, "title": "The Hobbit", "sort_title": "hobbit"})
        );
    }
}
