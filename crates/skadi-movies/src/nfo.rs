//! Kodi/Jellyfin `movie.nfo` generation (SKADI-T-0298).
//!
//! Written next to the placed video on import so a media-server scan matches the
//! movie **exactly** (tmdb + imdb `uniqueid`s) without re-scraping. Best-effort: the
//! library video file is the contract; the NFO rides along and a write failure never
//! fails the import. Unknown fields are simply omitted.

use crate::movie::Movie;

/// Escape the five XML predefined entities for element text / attribute values.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Render a Kodi/Jellyfin `<movie>` NFO document for `movie`.
#[must_use]
pub fn movie_nfo_xml(movie: &Movie) -> String {
    let mut x = String::with_capacity(512);
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<movie>\n");
    x.push_str(&format!("  <title>{}</title>\n", esc(&movie.title)));
    if let Some(o) = &movie.original_title {
        x.push_str(&format!("  <originaltitle>{}</originaltitle>\n", esc(o)));
    }
    if let Some(y) = movie.year {
        x.push_str(&format!("  <year>{y}</year>\n"));
    }
    if let Some(p) = &movie.overview {
        x.push_str(&format!("  <plot>{}</plot>\n", esc(p)));
    }
    if let Some(r) = movie.runtime_minutes {
        x.push_str(&format!("  <runtime>{r}</runtime>\n"));
    }
    if let Some(t) = &movie.external_ids.tmdb {
        x.push_str(&format!(
            "  <uniqueid type=\"tmdb\" default=\"true\">{}</uniqueid>\n",
            t.0
        ));
    }
    if let Some(i) = &movie.external_ids.imdb {
        x.push_str(&format!(
            "  <uniqueid type=\"imdb\">{}</uniqueid>\n",
            esc(&i.0)
        ));
    }
    if let Some(u) = &movie.poster_url {
        x.push_str(&format!("  <thumb aspect=\"poster\">{}</thumb>\n", esc(u)));
    }
    if let Some(u) = &movie.backdrop_url {
        x.push_str(&format!(
            "  <fanart>\n    <thumb>{}</thumb>\n  </fanart>\n",
            esc(u)
        ));
    }
    x.push_str("</movie>\n");
    x
}

/// The NFO path that pairs with a placed video file: same stem, `.nfo` extension.
#[must_use]
pub fn nfo_path_for(video: &std::path::Path) -> std::path::PathBuf {
    video.with_extension("nfo")
}

#[cfg(test)]
mod tests {
    use super::*;
    use skadi_core::{ExternalIds, ImdbId, ProfileId, RootFolder, RootFolderId, TmdbId};

    fn sample() -> Movie {
        let mut m = Movie::new(
            ExternalIds {
                tmdb: Some(TmdbId(10378)),
                imdb: Some(ImdbId("tt1254207".into())),
                ..Default::default()
            },
            "Big & Buck <Bunny>",
            ProfileId::new(),
            RootFolder {
                id: RootFolderId::new(),
                path: "/movies".into(),
            },
        );
        m.year = Some(2008);
        m.overview = Some("A \"giant\" rabbit.".into());
        m.runtime_minutes = Some(10);
        m
    }

    #[test]
    fn renders_dual_ids_and_escapes_xml() {
        let xml = movie_nfo_xml(&sample());
        assert!(xml.contains("<title>Big &amp; Buck &lt;Bunny&gt;</title>"));
        assert!(xml.contains("<year>2008</year>"));
        assert!(xml.contains(r#"<uniqueid type="tmdb" default="true">10378</uniqueid>"#));
        assert!(xml.contains(r#"<uniqueid type="imdb">tt1254207</uniqueid>"#));
        assert!(xml.contains("&quot;giant&quot;"));
        assert!(xml.trim_end().ends_with("</movie>"));
    }

    #[test]
    fn nfo_path_swaps_extension() {
        assert_eq!(
            nfo_path_for(std::path::Path::new("/m/the-matrix_(1999).mkv")),
            std::path::PathBuf::from("/m/the-matrix_(1999).nfo")
        );
    }
}
