//! Redaction of secrets from text that leaves the API (SKADI-T-0685).
//!
//! One function, [`redact`], for every place that serves text an operator can
//! read: health check messages and remediations (`/health/checks`), the lines of
//! the log ring (`/log`), and the target of the access log.
//!
//! Provider errors are the reason this exists. A Torznab indexer that cannot be
//! reached fails with reqwest's text, which names the whole request URL:
//!
//! ```text
//! error sending request for url (http://jackett:9117/api?t=caps&apikey=SECRET)
//! ```
//!
//! That text went into the `indexer:<name>` check as is, so anyone who could
//! read `/health/checks` could read the indexer's API key.
//!
//! ## What is masked
//!
//! - The value of a secret query key ([`SECRET_KEYS`]): `apikey=SECRET` →
//!   `apikey=***`. The key stays, because "the client sent an apikey" and "the
//!   client sent nothing" are different diagnoses.
//! - The same keys as JSON fields: `"password":"x"` → `"password":"***"`.
//! - The credentials in a URL: `http://user:pass@host` → `http://user:***@host`,
//!   and a bare `http://token@host` → `http://***@host`.
//! - The value of each configured provider secret ([`remember_secret`]),
//!   wherever it appears: an upstream can echo a key in any shape.
//!
//! Redaction is best effort over free text. It is a guard for the places where
//! text is shown, not a reason to put a secret into a message.

use std::borrow::Cow;
use std::sync::{LazyLock, RwLock};

use regex::Regex;

/// What a masked value reads as.
pub const MASK: &str = "***";

/// Query and field keys whose values are secrets. Matched case-insensitively,
/// as whole keys (`key` masks `key=` and `api-key=`, not `monkey=`).
pub const SECRET_KEYS: &[&str] = &[
    "apikey",
    "api_key",
    "api-key",
    "access_token",
    "token",
    "passkey",
    "key",
    "secret",
    "password",
    "passwd",
];

/// A configured secret shorter than this is not masked by value: masking a
/// three-letter password would mask that word in every line. The pattern rules
/// still mask it where it appears as a keyed value.
pub const MIN_SECRET_LEN: usize = 6;

/// How many secret values are kept. Old values (a rotated key) drop off first.
const MAX_SECRETS: usize = 256;

fn keys_alternation() -> String {
    SECRET_KEYS
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|")
}

/// `apikey=VALUE`, as a query parameter or in free text.
static KEYED_PARAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)\b({})=([^&\s"'<>\\)]+)"#,
        keys_alternation()
    ))
    .expect("the keyed-parameter pattern compiles")
});

/// `"apikey": "VALUE"` (JSON, or a Debug-formatted map with quoted keys).
static KEYED_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)(\\?"(?:{})\\?"\s*:\s*\\?")((?:[^"\\]|\\[^"])*)(\\?")"#,
        keys_alternation()
    ))
    .expect("the keyed-field pattern compiles")
});

/// `scheme://userinfo@`: group 1 the scheme, group 2 the user (if a password
/// follows), group 3 the user without a password.
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([a-z][a-z0-9+.-]*://)(?:([^:/?#@\s"'<>]*):[^/?#@\s"'<>]*|([^:/?#@\s"'<>]+))@"#,
    )
    .expect("the URL userinfo pattern compiles")
});

/// The configured secret values, longest first (so a secret that contains
/// another is masked whole).
static SECRETS: LazyLock<RwLock<Vec<String>>> = LazyLock::new(|| RwLock::new(Vec::new()));

/// Mask `secret` wherever it appears in redacted text from now on.
///
/// Call it where a provider secret is read from the credential vault. A JSON
/// object secret (the login settings of a cardigann tracker) is remembered
/// whole and by each of its string values. Values shorter than
/// [`MIN_SECRET_LEN`] are ignored. The value is never logged.
pub fn remember_secret(secret: &str) {
    let mut values = vec![secret.to_string()];
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(secret) {
        values.extend(map.values().filter_map(|v| v.as_str()).map(str::to_string));
    }
    let Ok(mut known) = SECRETS.write() else {
        return;
    };
    for v in values {
        let v = v.trim().to_string();
        if v.chars().count() < MIN_SECRET_LEN || known.contains(&v) {
            continue;
        }
        if known.len() >= MAX_SECRETS {
            // The list is longest-first, not oldest-first; dropping any one
            // entry is fine — this only bounds a list that rotation grows.
            known.pop();
        }
        known.push(v);
    }
    known.sort_by_key(|s| std::cmp::Reverse(s.len()));
}

/// `text` with every secret masked. Borrows when there is nothing to mask.
#[must_use]
pub fn redact(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);
    if let Ok(known) = SECRETS.read() {
        for secret in known.iter() {
            if out.contains(secret.as_str()) {
                out = Cow::Owned(out.replace(secret.as_str(), MASK));
            }
        }
    }
    let out = replace(out, &URL_USERINFO, |c| match (c.get(2), c.get(3)) {
        (Some(user), _) => format!("{}{}:{MASK}@", &c[1], user.as_str()),
        _ => format!("{}{MASK}@", &c[1]),
    });
    let out = replace(out, &KEYED_PARAM, |c| format!("{}={MASK}", &c[1]));
    replace(out, &KEYED_FIELD, |c| format!("{}{MASK}{}", &c[1], &c[3]))
}

/// [`redact`] into an owned `String`.
#[must_use]
pub fn redact_string(text: String) -> String {
    match redact(&text) {
        Cow::Borrowed(_) => text,
        Cow::Owned(s) => s,
    }
}

fn replace<'a>(
    text: Cow<'a, str>,
    re: &Regex,
    rep: impl Fn(&regex::Captures<'_>) -> String,
) -> Cow<'a, str> {
    if !re.is_match(&text) {
        return text;
    }
    Cow::Owned(
        re.replace_all(&text, |c: &regex::Captures<'_>| rep(c))
            .into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_secret_query_key_is_masked() {
        for k in SECRET_KEYS {
            let out = redact_string(format!("/api?t=caps&{k}=leak-me&limit=5"));
            assert_eq!(out, format!("/api?t=caps&{k}=***&limit=5"), "{k}");
        }
    }

    #[test]
    fn keys_match_case_insensitively_and_whole() {
        assert_eq!(redact("ApiKey=abc123"), "ApiKey=***");
        assert_eq!(redact("PASSKEY=abc123"), "PASSKEY=***");
        // Not a secret key: a key that only ends in one.
        assert_eq!(
            redact("monkey=banana&sort_key=name"),
            "monkey=banana&sort_key=name"
        );
    }

    #[test]
    fn the_reqwest_error_of_an_unreachable_indexer_is_masked() {
        let raw = "Network error: error sending request for url (http://jackett:9117/api/v2.0/indexers/all/results/torznab/api?t=caps&apikey=0123456789abcdef)";
        let out = redact(raw);
        assert!(!out.contains("0123456789abcdef"), "{out}");
        assert!(
            out.ends_with("torznab/api?t=caps&apikey=***)"),
            "the closing parenthesis is not part of the value: {out}"
        );
    }

    #[test]
    fn a_keyed_value_in_a_debug_formatted_field_stops_at_the_escaped_quote() {
        let out = redact(r#"error="GET http://x/api?apikey=s3cr3t\" failed""#);
        assert_eq!(out, r#"error="GET http://x/api?apikey=***\" failed""#);
    }

    #[test]
    fn json_secret_fields_are_masked() {
        let out = redact(r#"{"name":"qbit","password":"hunter2","api_key": "abc"}"#);
        assert_eq!(out, r#"{"name":"qbit","password":"***","api_key": "***"}"#);
    }

    #[test]
    fn the_password_in_a_url_is_masked_and_the_user_kept() {
        assert_eq!(
            redact("connecting to http://admin:hunter2@qbit:8080/api/v2"),
            "connecting to http://admin:***@qbit:8080/api/v2"
        );
        assert_eq!(
            redact("socks5://u:p@proxy:1080"),
            "socks5://u:***@proxy:1080"
        );
    }

    #[test]
    fn a_token_as_the_url_user_is_masked() {
        assert_eq!(
            redact("https://ghp_tokentoken@github.com/x"),
            "https://***@github.com/x"
        );
    }

    #[test]
    fn an_email_address_or_a_plain_url_is_not_touched() {
        let t = "mail ops@example.com about http://host:8080/path?x=1";
        assert_eq!(redact(t), t);
        assert!(matches!(redact(t), Cow::Borrowed(_)), "no copy when clean");
    }

    #[test]
    fn a_remembered_secret_is_masked_anywhere() {
        remember_secret("Zq7-remembered-value");
        assert_eq!(
            redact("the tracker said: bad key Zq7-remembered-value (code 100)"),
            "the tracker said: bad key *** (code 100)"
        );
    }

    #[test]
    fn a_json_object_secret_is_remembered_by_each_value() {
        remember_secret(r#"{"username":"cardi-user-77","password":"cardi-pass-77"}"#);
        assert_eq!(
            redact("login failed for cardi-user-77 / cardi-pass-77"),
            "login failed for *** / ***"
        );
    }

    #[test]
    fn a_short_secret_is_not_masked_by_value() {
        remember_secret("abc");
        assert_eq!(redact("abc def"), "abc def");
    }

    #[test]
    fn a_longer_secret_that_contains_a_shorter_one_is_masked_whole() {
        remember_secret("inner-secret-1");
        remember_secret("outer-inner-secret-1-x");
        assert_eq!(redact("[outer-inner-secret-1-x]"), "[***]");
    }

    #[test]
    fn redact_string_keeps_a_clean_string() {
        assert_eq!(redact_string("all clear".into()), "all clear");
        assert_eq!(redact_string("token=t0k3n".into()), "token=***");
    }
}
