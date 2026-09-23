//! Minimal reader for Kodi/Jellyfin `.nfo` sidecar metadata (SKADI-T-0324).
//!
//! Library-import uses these as **authoritative** parse hints: a show's
//! `tvshow.nfo` / a movie's `movie.nfo` carries the real title and the exact
//! provider ids (`<uniqueid type="tvdb"|"tmdb"|"imdb">…</uniqueid>`), which beats
//! guessing from a messy folder name and lets us skip a fuzzy metadata search.
//!
//! Deliberately a tiny string scanner, not a full XML parse: NFO files are flat,
//! well-formed enough, and we only need a couple of leaf values. Tolerant of
//! attributes, single/double quotes, and surrounding whitespace.

/// The text of `<uniqueid type="{kind}">…</uniqueid>` (e.g. `kind = "tvdb"`),
/// trimmed. `None` if absent. Matches the first `uniqueid` of that type.
#[must_use]
pub fn uniqueid(xml: &str, kind: &str) -> Option<String> {
    let needle_dq = format!("type=\"{kind}\"");
    let needle_sq = format!("type='{kind}'");
    let mut rest = xml;
    while let Some(start) = rest.find("<uniqueid") {
        let after = &rest[start..];
        let Some(gt) = after.find('>') else { break };
        let open_tag = &after[..gt];
        let Some(close) = after.find("</uniqueid>") else {
            break;
        };
        if gt < close && (open_tag.contains(&needle_dq) || open_tag.contains(&needle_sq)) {
            let value = after[gt + 1..close].trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
        rest = &after[close + "</uniqueid>".len()..];
    }
    None
}

/// A `uniqueid` parsed as a `u64` (provider numeric ids: tvdb/tmdb).
#[must_use]
pub fn uniqueid_u64(xml: &str, kind: &str) -> Option<u64> {
    uniqueid(xml, kind).and_then(|v| v.trim().parse().ok())
}

/// The text of the first `<{tag}>…</{tag}>` leaf (e.g. `title`, `year`), trimmed.
/// Ignores tags with attributes only when the open tag isn't a bare `<tag>` — good
/// enough for the flat `title`/`year` leaves we read.
#[must_use]
pub fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let closeb = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&closeb)? + start;
    let value = xml[start..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Every `<{tag}>…</{tag}>` leaf's text, in order (e.g. the repeated `<genre>`
/// tags). Empty when none are present.
#[must_use]
pub fn tags(xml: &str, name: &str) -> Vec<String> {
    let open = format!("<{name}>");
    let closeb = format!("</{name}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(s) = rest.find(&open) {
        let after = &rest[s + open.len()..];
        let Some(e) = after.find(&closeb) else { break };
        let value = after[..e].trim();
        if !value.is_empty() {
            out.push(value.to_string());
        }
        rest = &after[e + closeb.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TV: &str = r#"<tvshow>
  <title>The Leftovers</title>
  <year>2014</year>
  <genre>Drama</genre>
  <genre>Fantasy</genre>
  <uniqueid type="tvdb" default="true">269689</uniqueid>
  <uniqueid type="imdb">tt2699128</uniqueid>
</tvshow>"#;

    const MOVIE: &str = r#"<movie>
  <title>Rebel Moon - Part One: A Child of Fire</title>
  <uniqueid type="tmdb" default="true">848326</uniqueid>
  <uniqueid type="imdb">tt14998742</uniqueid>
</movie>"#;

    #[test]
    fn reads_uniqueids_and_leaf_tags() {
        assert_eq!(uniqueid_u64(TV, "tvdb"), Some(269689));
        assert_eq!(uniqueid(TV, "imdb").as_deref(), Some("tt2699128"));
        assert_eq!(tag(TV, "title").as_deref(), Some("The Leftovers"));
        assert_eq!(tag(TV, "year").as_deref(), Some("2014"));

        assert_eq!(uniqueid_u64(MOVIE, "tmdb"), Some(848326));
        assert_eq!(
            tag(MOVIE, "title").as_deref(),
            Some("Rebel Moon - Part One: A Child of Fire")
        );

        // Absent ids / tags are None, not a panic.
        assert_eq!(uniqueid(TV, "tmdb"), None);
        assert_eq!(tag(MOVIE, "year"), None);

        // Repeated tags collect in order; absent → empty.
        assert_eq!(tags(TV, "genre"), vec!["Drama", "Fantasy"]);
        assert!(tags(MOVIE, "genre").is_empty());
    }
}
