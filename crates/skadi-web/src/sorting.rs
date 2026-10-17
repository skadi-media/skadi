//! Title ordering (SKADI-T-0648, SKADI-T-0737). The server emits the sort key
//! of each list record (`sort_title`; `sort_name` and `sort_authors` for
//! people), derived by `skadi_core::sorting`. The web orders by that key and
//! holds no copy of the rule.
//!
//! A record from an older server has no key. It still orders: [`key_or_raw`]
//! falls back to the raw title, trimmed and lowercased.
//!
//! The rail letter is the first character of the key ([`letter_of_key`]), so
//! a surface cannot sort by one rule and label by another, which is how the
//! Android authors list went wrong (SKADI-T-0648 F2).

/// The key a record sorts by: the one the server emitted, else the raw title
/// trimmed and lowercased.
#[must_use]
pub fn key_or_raw(key: Option<&str>, raw: &str) -> String {
    match key {
        Some(k) if !k.is_empty() || raw.trim().is_empty() => k.to_string(),
        _ => raw.trim().to_lowercase(),
    }
}

/// The A–Z letter a key files under. A key that does not start with a latin
/// letter (digits, symbols, other scripts) files under `#`, one bucket rather
/// than many.
#[must_use]
pub fn letter_of_key(key: &str) -> char {
    match key.chars().next().map(|c| c.to_ascii_uppercase()) {
        Some(c) if c.is_ascii_uppercase() => c,
        _ => '#',
    }
}

/// Sort `items` in place by `key`.
pub fn sort_by_key<T>(items: &mut [T], key: impl Fn(&T) -> String) {
    items.sort_by_cached_key(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_emitted_key_wins() {
        assert_eq!(key_or_raw(Some("hobbit"), "The Hobbit"), "hobbit");
    }

    #[test]
    fn without_a_key_the_raw_title_orders() {
        assert_eq!(key_or_raw(None, "  The Hobbit "), "the hobbit");
        // An empty key from a blank title is still the key.
        assert_eq!(key_or_raw(Some(""), "  "), "");
        // An empty key for a real title is not trusted.
        assert_eq!(key_or_raw(Some(""), "Dune"), "dune");
    }

    #[test]
    fn letters_follow_the_key() {
        assert_eq!(letter_of_key("hobbit"), 'H');
        assert_eq!(letter_of_key("1984"), '#');
        assert_eq!(letter_of_key("¡three amigos!"), '#');
        assert_eq!(letter_of_key(""), '#');
    }

    #[test]
    fn sorts_by_the_key_not_the_display_title() {
        let mut items = vec![("The Hobbit", "hobbit"), ("Dune", "dune")];
        sort_by_key(&mut items, |(_, k)| (*k).to_string());
        assert_eq!(items[0].0, "Dune");
    }
}
