//! Contributor roles baked into author names (SKADI-T-0652).
//!
//! Upstream author strings carry the contributor's role as a suffix. Audnexus
//! returns, for *Dangerous Women* (B00GXJN3U6):
//!
//! ```text
//! authors: [{"name": "George R. R. Martin - editor"},
//!           {"name": "Gardner Dozois - editor"}]
//! ```
//!
//! Stored as-is, the same person became several authors ("George R. R. Martin"
//! and "George R. R. Martin - editor"), translators and introduction writers
//! appeared as authors, and an author record was registered under the literal
//! string "Gardner Dozois - editor". Both providers' mappers now pass their
//! contributor lists through [`select_authors`], so this is the one place the
//! rule lives.

/// A contributor role that upstream appends to a name as ` - <role>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContributorRole {
    Editor,
    Translator,
    Introduction,
    Foreword,
    Afterword,
    Contributor,
    Illustrator,
    Adaptation,
    Adapter,
    Compiler,
    Preface,
    Narrator,
}

impl ContributorRole {
    fn parse(word: &str) -> Option<Self> {
        Some(match word.trim().to_lowercase().as_str() {
            "editor" => Self::Editor,
            "translator" => Self::Translator,
            "introduction" => Self::Introduction,
            "foreword" => Self::Foreword,
            "afterword" => Self::Afterword,
            "contributor" => Self::Contributor,
            "illustrator" => Self::Illustrator,
            "adaptation" => Self::Adaptation,
            "adapter" => Self::Adapter,
            "compiler" => Self::Compiler,
            "preface" => Self::Preface,
            "narrator" => Self::Narrator,
            _ => return None,
        })
    }

    /// Roles that make someone the de-facto author of a book that has no plain
    /// author: an anthology is "by" its editors. A compiler is an editor by
    /// another name.
    fn is_editorial(self) -> bool {
        matches!(self, Self::Editor | Self::Compiler)
    }
}

/// Split a trailing ` - <role>` off a contributor string.
///
/// Only **known** roles are stripped, and only when separated by a spaced dash.
/// Anything else is left exactly as it is — a hyphen inside a name
/// ("Jean-Paul Sartre") is not spaced, and an unknown suffix might be part of the
/// name, so guessing would damage real names. A suffix listing several roles
/// ("editor, translator") is stripped when every part is a known role; the first
/// is returned. The name is trimmed either way.
#[must_use]
pub fn split_role(raw: &str) -> (String, Option<ContributorRole>) {
    let trimmed = raw.trim();
    for sep in [" - ", " – ", " — "] {
        if let Some(at) = trimmed.rfind(sep) {
            let name = trimmed[..at].trim();
            let suffix = &trimmed[at + sep.len()..];
            let parts: Vec<&str> = suffix
                .split([',', '&', '/'])
                .flat_map(|p| p.split(" and "))
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect();
            // "author" is a recognised word too, and it wins: a suffix such as
            // "editor/author" — a real string Audible returns for George R. R.
            // Martin, found by the live check in SKADI-T-0650 — means the person
            // IS an author, so they are kept as a plain one.
            let is_author = |p: &str| p.trim().eq_ignore_ascii_case("author");
            let all_known = parts
                .iter()
                .all(|p| is_author(p) || ContributorRole::parse(p).is_some());
            if all_known && !parts.is_empty() && !name.is_empty() {
                if parts.iter().any(|p| is_author(p)) {
                    return (name.to_string(), None);
                }
                let first = ContributorRole::parse(parts[0]).expect("checked above");
                return (name.to_string(), Some(first));
            }
            break;
        }
    }
    (trimmed.to_string(), None)
}

/// Choose a book's authors from its raw contributor list, carrying each
/// author's companion value (`T` — an ASIN, or `()`) alongside so the two
/// cannot fall out of step.
///
/// - Plain contributors (no role) are the authors.
/// - If there are **none**, the editorial contributors (editor, compiler) are —
///   role-stripped. *Dangerous Women* lists only its two editors, and would
///   otherwise fall under "Unknown author".
/// - Translators, introductions, forewords and every other role are never
///   authors.
/// - Order is kept; duplicates (case-insensitive, after stripping) are dropped,
///   keeping the first, so "X" and "X - editor" on one book are one author.
pub fn select_authors<T>(contributors: impl IntoIterator<Item = (String, T)>) -> Vec<(String, T)> {
    let mut plain = Vec::new();
    let mut editorial = Vec::new();
    for (raw, extra) in contributors {
        let (name, role) = split_role(&raw);
        if name.is_empty() {
            continue;
        }
        match role {
            None => plain.push((name, extra)),
            Some(r) if r.is_editorial() => editorial.push((name, extra)),
            Some(_) => {}
        }
    }
    let chosen = if plain.is_empty() { editorial } else { plain };
    let mut seen = std::collections::HashSet::new();
    chosen
        .into_iter()
        .filter(|(n, _)| seen.insert(n.to_lowercase()))
        .collect()
}

/// [`select_authors`] for a plain list of names.
#[must_use]
pub fn select_author_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    select_authors(names.into_iter().map(|n| (n, ())))
        .into_iter()
        .map(|(n, ())| n)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ContributorRole as R;

    #[test]
    fn strips_each_known_role() {
        for (word, role) in [
            ("editor", R::Editor),
            ("translator", R::Translator),
            ("introduction", R::Introduction),
            ("foreword", R::Foreword),
            ("afterword", R::Afterword),
            ("contributor", R::Contributor),
            ("illustrator", R::Illustrator),
            ("adaptation", R::Adaptation),
            ("adapter", R::Adapter),
            ("compiler", R::Compiler),
            ("preface", R::Preface),
            ("narrator", R::Narrator),
        ] {
            assert_eq!(
                split_role(&format!("Jo March - {word}")),
                ("Jo March".to_string(), Some(role)),
                "{word}"
            );
        }
    }

    #[test]
    fn role_matching_ignores_case() {
        assert_eq!(
            split_role("Gardner Dozois - Editor"),
            ("Gardner Dozois".into(), Some(R::Editor))
        );
        assert_eq!(
            split_role("Joel Martinsen - TRANSLATOR"),
            ("Joel Martinsen".into(), Some(R::Translator))
        );
    }

    #[test]
    fn a_hyphen_inside_a_real_name_is_left_alone() {
        assert_eq!(
            split_role("Jean-Paul Sartre"),
            ("Jean-Paul Sartre".into(), None)
        );
        assert_eq!(
            split_role("Kim Stanley-Robinson"),
            ("Kim Stanley-Robinson".into(), None)
        );
    }

    #[test]
    fn an_unknown_suffix_is_left_alone() {
        // Might be part of the name; guessing would damage it.
        assert_eq!(
            split_role("Prince - Remastered"),
            ("Prince - Remastered".into(), None)
        );
        assert_eq!(split_role("A - B"), ("A - B".into(), None));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            split_role("  George R. R. Martin  -  editor  "),
            ("George R. R. Martin".into(), Some(R::Editor))
        );
        assert_eq!(split_role("  Stephen King "), ("Stephen King".into(), None));
    }

    #[test]
    fn a_list_of_known_roles_is_stripped() {
        assert_eq!(
            split_role("Ken Liu - editor, translator"),
            ("Ken Liu".into(), Some(R::Editor))
        );
        assert_eq!(
            split_role("Ken Liu - translator and editor"),
            ("Ken Liu".into(), Some(R::Translator))
        );
        // One unknown word in the list → not a role suffix after all.
        assert_eq!(
            split_role("Ken Liu - editor, genius"),
            ("Ken Liu - editor, genius".into(), None)
        );
    }

    /// The live Audible catalog returns "George R. R. Martin - editor/author"
    /// (found by the SKADI-T-0650 live check). "author" in the suffix means the
    /// person is an author: strip the suffix, keep them as a plain author.
    #[test]
    fn an_author_credit_in_the_suffix_makes_them_a_plain_author() {
        assert_eq!(
            split_role("George R. R. Martin - editor/author"),
            ("George R. R. Martin".into(), None)
        );
        assert_eq!(split_role("Jo March - Author"), ("Jo March".into(), None));
        assert_eq!(
            split_role("Jo March - author, editor"),
            ("Jo March".into(), None)
        );
        assert_eq!(
            select_author_names([
                "George R. R. Martin - editor/author".to_string(),
                "Gardner Dozois - editor".to_string(),
            ]),
            vec!["George R. R. Martin"],
            "he is a plain author, so the editor-only fallback does not apply"
        );
    }

    #[test]
    fn a_bare_role_with_no_name_is_not_stripped_to_nothing() {
        assert_eq!(split_role(" - editor"), ("- editor".into(), None));
    }

    #[test]
    fn an_anthology_with_only_editors_is_by_its_editors() {
        assert_eq!(
            select_author_names([
                "George R. R. Martin - editor".to_string(),
                "Gardner Dozois - editor".to_string(),
            ]),
            vec!["George R. R. Martin", "Gardner Dozois"]
        );
    }

    #[test]
    fn a_translator_is_never_an_author() {
        assert_eq!(
            select_author_names([
                "Cixin Liu".to_string(),
                "Joel Martinsen - translator".to_string()
            ]),
            vec!["Cixin Liu"]
        );
    }

    #[test]
    fn plain_authors_win_over_editors() {
        assert_eq!(
            select_author_names([
                "Joe Hill - introduction".to_string(),
                "Stephen King".to_string(),
                "Ellen Datlow - editor".to_string(),
            ]),
            vec!["Stephen King"]
        );
    }

    #[test]
    fn the_same_person_in_two_roles_is_one_author() {
        assert_eq!(
            select_author_names([
                "Jim Butcher".to_string(),
                "jim butcher - editor".to_string()
            ]),
            vec!["Jim Butcher"],
            "plain wins, and the editor credit does not add a second author"
        );
        assert_eq!(
            select_author_names(["A - editor".to_string(), "a - editor".to_string()]),
            vec!["A"]
        );
    }

    #[test]
    fn the_companion_value_stays_with_its_author() {
        // The editor comes first in the raw list; the plain author's ASIN must
        // be the one paired with authors[0].
        let picked = select_authors([
            ("Ellen Datlow - editor".to_string(), Some("EDITOR_ASIN")),
            ("Stephen King".to_string(), Some("KING_ASIN")),
        ]);
        assert_eq!(
            picked,
            vec![("Stephen King".to_string(), Some("KING_ASIN"))]
        );
    }

    #[test]
    fn only_non_author_roles_leaves_no_authors() {
        assert!(select_author_names(["Joel Martinsen - translator".to_string()]).is_empty());
    }
}
