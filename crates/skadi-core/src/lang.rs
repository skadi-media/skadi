//! Language codes (SKADI-T-0673).

/// A language code as ISO 639-1 (SKADI-T-0673). Sources disagree: TMDB and
/// Radarr's API send `en`, Skyhook sends `eng`, and a Matroska track says
/// `eng` or `ger`. Two letters pass through (lowercased); the common
/// three-letter codes (both 639-2 forms) map to two; anything else, `und`
/// included, is `None`. Pure.
#[must_use]
pub fn iso639_1(code: &str) -> Option<String> {
    let c = code.trim().to_ascii_lowercase();
    if c.len() == 2 && c.chars().all(|ch| ch.is_ascii_lowercase()) {
        return Some(c);
    }
    let two = match c.as_str() {
        "eng" => "en",
        "kor" => "ko",
        "jpn" => "ja",
        "zho" | "chi" => "zh",
        "fra" | "fre" => "fr",
        "deu" | "ger" => "de",
        "ita" => "it",
        "spa" => "es",
        "por" => "pt",
        "rus" => "ru",
        "ukr" => "uk",
        "pol" => "pl",
        "ces" | "cze" => "cs",
        "hun" => "hu",
        "tur" => "tr",
        "hin" => "hi",
        "tam" => "ta",
        "tel" => "te",
        "swe" => "sv",
        "dan" => "da",
        "nor" | "nob" | "nno" => "no",
        "fin" => "fi",
        "nld" | "dut" => "nl",
        "tha" => "th",
        "vie" => "vi",
        "ara" => "ar",
        "heb" => "he",
        "ell" | "gre" => "el",
        "ind" => "id",
        "isl" | "ice" => "is",
        "ron" | "rum" => "ro",
        _ => return None,
    };
    Some(two.to_string())
}

#[cfg(test)]
mod iso639_tests {
    use super::iso639_1;

    /// SKADI-T-0673: the codes each source sends map to one form.
    #[test]
    fn language_codes_normalise_to_two_letters() {
        for (code, want) in [
            ("en", Some("en")),
            ("EN", Some("en")),
            ("eng", Some("en")),
            ("kor", Some("ko")),
            ("ger", Some("de")),
            ("deu", Some("de")),
            ("fre", Some("fr")),
            ("und", None),
            ("", None),
            ("xx1", None),
        ] {
            assert_eq!(iso639_1(code).as_deref(), want, "{code}");
        }
    }
}
