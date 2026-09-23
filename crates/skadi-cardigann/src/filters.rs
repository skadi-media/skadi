//! The Cardigann selector/keyword **filter pipeline** (SKADI-T-0256): the typed
//! string transformations a definition applies to extracted values
//! (`re_replace`/`replace`/`split`/`regexp`/`append`/`prepend`/`trim`/`tolower`/
//! `urldecode`/`dateparse`/`fuzzytime`/`validate`/…). Pure + total; an unsupported
//! filter surfaces a [`FilterError`] so the caller can warn-skip the field/def.

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, TimeZone, Utc};
use percent_encoding::percent_decode_str;
use regex::Regex;

use crate::model::{Filter, Yaml};

/// Clock/context a filter pipeline runs against (relative-date filters need "now").
#[derive(Debug, Clone, Copy)]
pub struct FilterCtx {
    pub now: DateTime<Utc>,
}

impl Default for FilterCtx {
    fn default() -> Self {
        Self { now: Utc::now() }
    }
}

/// A filter that could not run (unknown name, bad args, unparseable date).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterError(pub String);

impl std::fmt::Display for FilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "filter error: {}", self.0)
    }
}
impl std::error::Error for FilterError {}

/// Apply a pipeline of filters left to right.
///
/// # Errors
/// Propagates the first [`FilterError`].
pub fn apply_all(filters: &[Filter], input: &str, ctx: &FilterCtx) -> Result<String, FilterError> {
    let mut s = input.to_string();
    for f in filters {
        s = apply(&f.name, &f.args, &s, ctx)?;
    }
    Ok(s)
}

/// Apply one named filter.
///
/// # Errors
/// [`FilterError`] for an unknown filter, bad args, or an unparseable date.
pub fn apply(name: &str, args: &Yaml, input: &str, ctx: &FilterCtx) -> Result<String, FilterError> {
    let a = args_vec(args);
    let arg = |i: usize| a.get(i).cloned().unwrap_or_default();
    match name {
        "re_replace" => match compile(&arg(0)) {
            Some(re) => Ok(re
                .replace_all(input, goish_replacement(&arg(1)).as_str())
                .into_owned()),
            // A pattern Rust's regex can't compile (a .NET-only construct we can't
            // translate) is treated as a no-op — best-effort, never aborts a search.
            None => Ok(input.to_string()),
        },
        "replace" => Ok(input.replace(&arg(0), &arg(1))),
        "split" => {
            let sep = arg(0);
            let parts: Vec<&str> = input.split(sep.as_str()).collect();
            let idx: i64 = arg(1)
                .trim()
                .parse()
                .map_err(|_| FilterError(format!("split: non-integer index '{}'", arg(1))))?;
            let i = if idx < 0 {
                parts.len() as i64 + idx
            } else {
                idx
            };
            parts
                .get(usize::try_from(i).unwrap_or(usize::MAX))
                .map(|s| (*s).to_string())
                .ok_or_else(|| FilterError(format!("split: index {idx} out of range")))
        }
        "trim" => {
            if a.is_empty() {
                Ok(input.trim().to_string())
            } else {
                let chars: Vec<char> = arg(0).chars().collect();
                Ok(input.trim_matches(|c| chars.contains(&c)).to_string())
            }
        }
        "prepend" => Ok(format!("{}{input}", arg(0))),
        "append" => Ok(format!("{input}{}", arg(0))),
        "tolower" => Ok(input.to_lowercase()),
        "toupper" => Ok(input.to_uppercase()),
        "urldecode" => Ok(percent_decode_str(input).decode_utf8_lossy().into_owned()),
        "htmldecode" => Ok(html_decode(input)),
        "regexp" => {
            // Extract: the first capture group if the pattern has one, else the
            // whole match; empty string if no match (cardigann semantics).
            let Some(re) = compile(&arg(0)) else {
                return Ok(String::new());
            };
            Ok(re
                .captures(input)
                .map(|c| {
                    c.get(1)
                        .or_else(|| c.get(0))
                        .map_or(String::new(), |m| m.as_str().to_string())
                })
                .unwrap_or_default())
        }
        "querystring" => Ok(query_param(input, &arg(0))),
        // Dates → RFC3339 (UTC); best-effort, refined against real data in T-0257.
        "dateparse" | "dateparse_text" => date_parse(input, &arg(0)),
        "fuzzytime" | "reltime" | "timeago" => fuzzy_time(input, ctx.now),
        // No-op-ish passthroughs: validation / known-but-output-neutral filters.
        "validate" | "diacritics" => Ok(input.to_string()),
        other => Err(FilterError(format!("unsupported filter '{other}'"))),
    }
}

/// Flatten a YAML filter-arg value into a list of strings (`"x"` → `["x"]`,
/// `["/", 3]` → `["/", "3"]`, `null` → `[]`).
fn args_vec(args: &Yaml) -> Vec<String> {
    match args {
        Yaml::String(s) => vec![s.clone()],
        Yaml::Number(n) => vec![n.to_string()],
        Yaml::Bool(b) => vec![b.to_string()],
        Yaml::Sequence(seq) => seq.iter().map(scalar).collect(),
        _ => Vec::new(),
    }
}

fn scalar(v: &Yaml) -> String {
    match v {
        Yaml::String(s) => s.clone(),
        Yaml::Number(n) => n.to_string(),
        Yaml::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// Compile a definition's regex, best-effort: try it verbatim, then a small
/// `.NET → Rust` Unicode-property translation, else `None` (the caller no-ops).
/// Cardigann patterns are written for .NET, which names scripts/blocks `Is<Name>`;
/// Rust's `regex` uses the bare script name.
fn compile(pattern: &str) -> Option<Regex> {
    if let Ok(re) = Regex::new(pattern) {
        return Some(re);
    }
    let translated = pattern
        .replace("IsCJKUnifiedIdeographs", "Han")
        .replace("IsCyrillic", "Cyrillic")
        .replace("IsGreek", "Greek")
        .replace("IsHebrew", "Hebrew")
        .replace("IsArabic", "Arabic")
        .replace("IsThai", "Thai")
        .replace("IsHiragana", "Hiragana")
        .replace("IsKatakana", "Katakana");
    Regex::new(&translated).ok()
}

/// Rewrite Go-style replacement group refs (`$1`) to Rust's unambiguous `${1}`,
/// so `$1foo` doesn't read as group "1foo".
fn goish_replacement(repl: &str) -> String {
    // `$1` / `$12` → `${1}` / `${12}`; leave `${..}` and `$$` alone.
    let re = Regex::new(r"\$(\d+)").unwrap();
    re.replace_all(repl, "$${$1}").into_owned()
}

fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

fn query_param(url: &str, key: &str) -> String {
    let q = url.split_once('?').map_or(url, |(_, q)| q);
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return percent_decode_str(v).decode_utf8_lossy().into_owned();
        }
    }
    String::new()
}

/// Parse `input` with a **Go** time layout, output RFC3339 (UTC).
fn date_parse(input: &str, go_layout: &str) -> Result<String, FilterError> {
    let input = input.trim();
    // Unix epoch shortcuts the definitions use directly.
    if go_layout.is_empty()
        && let Ok(secs) = input.parse::<i64>()
    {
        return Ok(Utc
            .timestamp_opt(secs, 0)
            .single()
            .map_or_else(|| input.to_string(), |dt| dt.to_rfc3339()));
    }
    let fmt = go_to_chrono(go_layout);
    // Try datetime, then date-only (midnight UTC).
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, &fmt) {
        return Ok(Utc.from_utc_datetime(&dt).to_rfc3339());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(input, &fmt) {
        return Ok(Utc
            .from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .to_rfc3339());
    }
    Err(FilterError(format!(
        "dateparse: '{input}' does not match layout '{go_layout}'"
    )))
}

/// Translate the Go reference-time layout into a chrono `strftime` format.
fn go_to_chrono(go: &str) -> String {
    // Longest-token-first so e.g. "2006" matches before "06".
    const MAP: &[(&str, &str)] = &[
        ("2006", "%Y"),
        ("January", "%B"),
        ("Monday", "%A"),
        ("-07:00", "%:z"),
        ("-0700", "%z"),
        ("15", "%H"),
        ("Jan", "%b"),
        ("Mon", "%a"),
        ("MST", "%Z"),
        ("PM", "%p"),
        ("01", "%m"),
        ("02", "%d"),
        ("03", "%I"),
        ("04", "%M"),
        ("05", "%S"),
        ("06", "%y"),
        ("_2", "%e"),
    ];
    let mut out = String::new();
    let mut rest = go;
    'outer: while !rest.is_empty() {
        for (go_tok, c_tok) in MAP {
            if rest.starts_with(go_tok) {
                out.push_str(c_tok);
                rest = &rest[go_tok.len()..];
                continue 'outer;
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// Parse a relative/fuzzy time ("2 hours ago", "Today", "Yesterday", "5 min ago")
/// into RFC3339, relative to `now`.
fn fuzzy_time(input: &str, now: DateTime<Utc>) -> Result<String, FilterError> {
    let s = input.trim().to_lowercase();
    match s.as_str() {
        "now" | "just now" => return Ok(now.to_rfc3339()),
        "today" => return Ok(now.to_rfc3339()),
        "yesterday" => return Ok((now - Duration::days(1)).to_rfc3339()),
        _ => {}
    }
    // "<n> <unit>[s] ago"
    let re = Regex::new(r"(?i)(\d+)\s*(sec|min|hour|day|week|month|year)").unwrap();
    if let Some(c) = re.captures(&s) {
        let n: i64 = c[1].parse().unwrap_or(0);
        let dur = match &c[2] {
            "sec" => Duration::seconds(n),
            "min" => Duration::minutes(n),
            "hour" => Duration::hours(n),
            "day" => Duration::days(n),
            "week" => Duration::weeks(n),
            "month" => Duration::days(30 * n),
            "year" => Duration::days(365 * n),
            _ => Duration::zero(),
        };
        let when = now - dur;
        // Cardigann "fuzzy" dates lose sub-day precision for day+ units.
        let when = if matches!(&c[2], "day" | "week" | "month" | "year") {
            when.with_time(chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap())
                .single()
                .unwrap_or(when)
        } else {
            when
        };
        let _ = when.year(); // touch Datelike to keep the import meaningful
        return Ok(when.to_rfc3339());
    }
    Err(FilterError(format!("fuzzytime: unrecognised '{input}'")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::Value as Y;

    fn ctx() -> FilterCtx {
        FilterCtx {
            now: Utc.with_ymd_and_hms(2026, 6, 20, 12, 0, 0).unwrap(),
        }
    }
    fn list(items: &[&str]) -> Y {
        Y::Sequence(items.iter().map(|s| Y::String((*s).into())).collect())
    }

    #[test]
    fn re_replace_with_group_refs() {
        // "Season 2" → "S02"
        let args = list(&[r"(?i)\bSeason[\s\.]+(\d)\b", "S0$1"]);
        assert_eq!(
            apply("re_replace", &args, "The Show Season 2", &ctx()).unwrap(),
            "The Show S02"
        );
    }

    #[test]
    fn replace_split_trim_affix() {
        assert_eq!(
            apply("replace", &list(&["-", " "]), "a-b-c", &ctx()).unwrap(),
            "a b c"
        );
        // split "/torrent/123/name" by "/" index 3 → "name"
        assert_eq!(
            apply("split", &list(&["/", "3"]), "/torrent/123/name", &ctx()).unwrap(),
            "name"
        );
        // negative index from the end
        assert_eq!(
            apply("split", &list(&["/", "-1"]), "a/b/c", &ctx()).unwrap(),
            "c"
        );
        assert_eq!(apply("trim", &Y::Null, "  hi  ", &ctx()).unwrap(), "hi");
        assert_eq!(
            apply("append", &Y::String("/1/".into()), "search/x", &ctx()).unwrap(),
            "search/x/1/"
        );
        assert_eq!(
            apply("prepend", &Y::String("/".into()), "x", &ctx()).unwrap(),
            "/x"
        );
    }

    #[test]
    fn regexp_extract_and_querystring_and_urldecode() {
        assert_eq!(
            apply(
                "regexp",
                &Y::String(r"tt(\d+)".into()),
                "imdb tt0133093 x",
                &ctx()
            )
            .unwrap(),
            "0133093"
        );
        assert_eq!(
            apply(
                "querystring",
                &Y::String("id".into()),
                "/d.php?id=42&x=1",
                &ctx()
            )
            .unwrap(),
            "42"
        );
        assert_eq!(
            apply("urldecode", &Y::Null, "a%20b%2Fc", &ctx()).unwrap(),
            "a b/c"
        );
    }

    #[test]
    fn dateparse_absolute_and_unix_and_fuzzy() {
        // Go layout
        let out = apply(
            "dateparse",
            &Y::String("2006-01-02 15:04:05".into()),
            "2024-03-01 08:30:00",
            &ctx(),
        )
        .unwrap();
        assert!(out.starts_with("2024-03-01T08:30:00"), "got {out}");
        // unix seconds (empty layout)
        let u = apply("dateparse", &Y::Null, "1700000000", &ctx()).unwrap();
        assert!(u.starts_with("2023-11-"), "got {u}");
        // relative
        let f = apply("fuzzytime", &Y::Null, "2 hours ago", &ctx()).unwrap();
        assert!(f.starts_with("2026-06-20T10:00:00"), "got {f}");
        assert!(
            apply("fuzzytime", &Y::Null, "yesterday", &ctx())
                .unwrap()
                .starts_with("2026-06-19")
        );
    }

    #[test]
    fn pipeline_and_unsupported() {
        let fs = vec![
            Filter {
                name: "trim".into(),
                args: Y::Null,
            },
            Filter {
                name: "tolower".into(),
                args: Y::Null,
            },
            Filter {
                name: "replace".into(),
                args: list(&[" ", "."]),
            },
        ];
        assert_eq!(
            apply_all(&fs, "  The Matrix  ", &ctx()).unwrap(),
            "the.matrix"
        );
        assert!(apply("no_such_filter", &Y::Null, "x", &ctx()).is_err());
    }
}
