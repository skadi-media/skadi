//! Title normalization for the text-search tier.
//!
//! Produces a search-friendly form of a title: lowercased, common Latin
//! diacritics folded to ASCII, `&` → "and", a leading "the" dropped, and all
//! runs of non-alphanumeric characters collapsed to single spaces. Shared by
//! the request generator's title tier (SKADI-T-0022).

/// Fold a small set of common Latin-1 accented letters to ASCII. (Full Unicode
/// normalization is a follow-up; this covers the common Western cases the
/// corpus and real release names exercise.)
fn fold_char(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        other => other,
    }
}

/// Normalize a title to a search form (see module docs).
#[must_use]
pub fn normalize_title(title: &str) -> String {
    let lowered: String = title.to_lowercase().chars().map(fold_char).collect();
    let with_and = lowered.replace('&', " and ");

    // Split on any non-alphanumeric, drop empties, then strip a leading "the".
    let mut words: Vec<&str> = with_and
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.first() == Some(&"the") {
        words.remove(0);
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_collapses_separators() {
        assert_eq!(normalize_title("The.Matrix.1999"), "matrix 1999");
        assert_eq!(normalize_title("Blade  Runner"), "blade runner");
    }

    #[test]
    fn drops_leading_the_only() {
        assert_eq!(normalize_title("The Thing"), "thing");
        // "the" mid-title is kept.
        assert_eq!(
            normalize_title("All the President's Men"),
            "all the president s men"
        );
    }

    #[test]
    fn folds_diacritics_and_ampersand() {
        assert_eq!(normalize_title("Amélie"), "amelie");
        assert_eq!(normalize_title("Fast & Furious"), "fast and furious");
        assert_eq!(normalize_title("Pokémon"), "pokemon");
    }

    #[test]
    fn punctuation_becomes_spaces() {
        assert_eq!(
            normalize_title("Spider-Man: No Way Home"),
            "spider man no way home"
        );
    }
}
