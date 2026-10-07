//! Secrets from files: the `X_FILE` convention (SKADI-T-0702).
//!
//! A secret passed as a plain environment variable shows in `docker inspect`
//! and in the environment of every process listing. The Docker convention is
//! to pass `X_FILE=/run/secrets/x` instead and read the value from that file.
//! [`env_or_file`] is the one place that rule lives: the daemon, the store and
//! the excluded download worker all resolve their environment through it.
//!
//! The rule is additive. A deploy that sets only `X` keeps working exactly as
//! before; `X_FILE` is consulted only when it is set.
//!
//! - `X` set, `X_FILE` unset: the value of `X`.
//! - `X_FILE` set, `X` unset: the contents of the file, trailing newline
//!   (`\n` or `\r\n`) removed — `echo secret > file` must not put a newline in
//!   a password.
//! - both set: an error. Picking one silently is how a rotated secret keeps
//!   being ignored.
//! - an empty value counts as unset, for both names. Compose writes `X: ""` for
//!   a `${X:-}` that has no value, and that must not conflict with `X_FILE`.
//!
//! No error message carries the value — only the variable name, the path, and
//! the reason the file could not be read.

use std::path::Path;

/// The suffix that names the file variant of an environment variable.
pub const FILE_SUFFIX: &str = "_FILE";

/// The env var holding the backing-store URL (Tier 0).
pub const DATABASE_URL_ENV: &str = "SKADI_DATABASE_URL";

/// The env var holding the database password, spliced into
/// [`DATABASE_URL_ENV`] by [`database_url`] so the URL itself carries none.
pub const DATABASE_PASSWORD_ENV: &str = "SKADI_DATABASE_PASSWORD";

/// Why an environment value could not be resolved. The messages never contain
/// the secret itself.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvError {
    /// `X` and `X_FILE` are both set.
    #[error("{name} and {name}_FILE are both set. Set only one of them.")]
    BothSet { name: String },
    /// `X_FILE` names a file that cannot be read.
    #[error("{name}_FILE names {path:?}, but the file cannot be read: {reason}")]
    Unreadable {
        name: String,
        path: String,
        reason: String,
    },
    /// The database URL and password do not combine.
    #[error("{0}")]
    DatabaseUrl(String),
}

/// Resolve `name` from the process environment, honouring `name_FILE`.
/// `Ok(None)` when neither is set (or both are empty).
pub fn env_or_file(name: &str) -> Result<Option<String>, EnvError> {
    env_or_file_with(
        name,
        |n| std::env::var(n).ok(),
        |p| std::fs::read_to_string(p),
    )
}

/// [`env_or_file`] over an injected environment and file reader, so the rule
/// is unit-testable without touching the process environment.
pub fn env_or_file_with(
    name: &str,
    get: impl Fn(&str) -> Option<String>,
    read: impl Fn(&Path) -> std::io::Result<String>,
) -> Result<Option<String>, EnvError> {
    let direct = get(name).filter(|v| !v.is_empty());
    let file = get(&format!("{name}{FILE_SUFFIX}")).filter(|v| !v.is_empty());
    match (direct, file) {
        (Some(_), Some(_)) => Err(EnvError::BothSet {
            name: name.to_string(),
        }),
        (Some(v), None) => Ok(Some(v)),
        (None, None) => Ok(None),
        (None, Some(path)) => {
            let contents = read(Path::new(&path)).map_err(|e| EnvError::Unreadable {
                name: name.to_string(),
                path: path.clone(),
                reason: e.to_string(),
            })?;
            Ok(Some(trim_newline(contents)))
        }
    }
}

/// Remove one trailing `\n` or `\r\n`. Only line endings: a secret may
/// legitimately end in a space.
fn trim_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

/// The backing-store URL from the environment, or `Ok(None)` when unset.
///
/// `SKADI_DATABASE_URL` (or `_FILE`) gives the URL. When
/// `SKADI_DATABASE_PASSWORD` (or `_FILE`) is set too, it is put into the URL's
/// user info, percent-encoded, so the URL in the environment can be
/// `postgres://skadi@postgres:5432/skadi` and the password can come from the
/// same secret file Postgres reads with `POSTGRES_PASSWORD_FILE`.
pub fn database_url() -> Result<Option<String>, EnvError> {
    database_url_with(|n| std::env::var(n).ok(), |p| std::fs::read_to_string(p))
}

/// [`database_url`] over an injected environment and file reader.
pub fn database_url_with(
    get: impl Fn(&str) -> Option<String>,
    read: impl Fn(&Path) -> std::io::Result<String>,
) -> Result<Option<String>, EnvError> {
    let url = env_or_file_with(DATABASE_URL_ENV, &get, &read)?;
    let password = env_or_file_with(DATABASE_PASSWORD_ENV, &get, &read)?;
    match (url, password) {
        (url, None) => Ok(url),
        (None, Some(_)) => Err(EnvError::DatabaseUrl(format!(
            "{DATABASE_PASSWORD_ENV} is set, but {DATABASE_URL_ENV} is not. \
             Set {DATABASE_URL_ENV} to the URL without the password."
        ))),
        (Some(url), Some(pw)) => with_password(&url, &pw).map(Some),
    }
}

/// Put `password` into the user info of `url`. The URL must name a user and
/// must not already hold a password.
fn with_password(url: &str, password: &str) -> Result<String, EnvError> {
    let bad = |why: &str| {
        EnvError::DatabaseUrl(format!(
            "{DATABASE_PASSWORD_ENV} is set, but {DATABASE_URL_ENV} {why}."
        ))
    };
    let Some(scheme_end) = url.find("://") else {
        return Err(bad("has no scheme"));
    };
    let rest_start = scheme_end + 3;
    let rest = &url[rest_start..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some(at) = authority.rfind('@') else {
        return Err(bad("names no user (write it as scheme://user@host/db)"));
    };
    let userinfo = &authority[..at];
    if userinfo.contains(':') {
        return Err(bad("already holds a password. Remove one of them"));
    }
    if userinfo.is_empty() {
        return Err(bad("names no user (write it as scheme://user@host/db)"));
    }
    let insert_at = rest_start + at;
    Ok(format!(
        "{}:{}{}",
        &url[..insert_at],
        percent_encode_userinfo(password),
        &url[insert_at..]
    ))
}

/// Percent-encode everything but RFC 3986 unreserved characters, so a password
/// holding `@`, `:` or `/` cannot change how the URL parses.
fn percent_encode_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The password in a URL's user info, as written (still percent-encoded), so
/// the daemon can add it to its redaction list. `None` when there is none.
#[must_use]
pub fn url_password(url: &str) -> Option<&str> {
    let rest = &url[url.find("://")? + 3..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let userinfo = &authority[..authority.rfind('@')?];
    let (_, pw) = userinfo.split_once(':')?;
    (!pw.is_empty()).then_some(pw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |n| map.get(n).cloned()
    }

    fn files(pairs: &[(&str, &str)]) -> impl Fn(&Path) -> io::Result<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |p| {
            map.get(p.to_str().unwrap())
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such file"))
        }
    }

    #[test]
    fn plain_variable_is_unchanged() {
        let got = env_or_file_with(
            "SKADI_API_TOKEN",
            env(&[("SKADI_API_TOKEN", "tok")]),
            files(&[]),
        );
        assert_eq!(got, Ok(Some("tok".into())));
    }

    #[test]
    fn file_variable_reads_the_file_and_trims_the_newline() {
        let got = env_or_file_with(
            "SKADI_API_TOKEN",
            env(&[("SKADI_API_TOKEN_FILE", "/run/secrets/t")]),
            files(&[("/run/secrets/t", "from-file\n")]),
        );
        assert_eq!(got, Ok(Some("from-file".into())));
    }

    #[test]
    fn crlf_is_trimmed_but_other_whitespace_is_kept() {
        let get = env(&[("X_FILE", "/f")]);
        assert_eq!(
            env_or_file_with("X", &get, files(&[("/f", "a b \r\n")])),
            Ok(Some("a b ".into()))
        );
        assert_eq!(
            env_or_file_with("X", &get, files(&[("/f", "no-newline")])),
            Ok(Some("no-newline".into()))
        );
    }

    #[test]
    fn both_set_is_an_error_that_does_not_print_the_value() {
        let got = env_or_file_with(
            "SKADI_SECRET_KEY",
            env(&[
                ("SKADI_SECRET_KEY", "hunter2-plain"),
                ("SKADI_SECRET_KEY_FILE", "/run/secrets/k"),
            ]),
            files(&[("/run/secrets/k", "hunter2-file\n")]),
        );
        let err = got.unwrap_err();
        assert_eq!(
            err,
            EnvError::BothSet {
                name: "SKADI_SECRET_KEY".into()
            }
        );
        let msg = err.to_string();
        assert!(msg.contains("SKADI_SECRET_KEY_FILE"), "{msg}");
        assert!(!msg.contains("hunter2"), "{msg}");
    }

    #[test]
    fn an_empty_plain_value_does_not_conflict_with_the_file() {
        // Compose renders `${X:-}` with no value as an empty string.
        let got = env_or_file_with(
            "X",
            env(&[("X", ""), ("X_FILE", "/f")]),
            files(&[("/f", "v\n")]),
        );
        assert_eq!(got, Ok(Some("v".into())));
        assert_eq!(
            env_or_file_with("X", env(&[("X", ""), ("X_FILE", "")]), files(&[])),
            Ok(None)
        );
    }

    #[test]
    fn unreadable_file_names_the_variable_and_path() {
        let err = env_or_file_with("X", env(&[("X_FILE", "/missing")]), files(&[])).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("X_FILE"), "{msg}");
        assert!(msg.contains("/missing"), "{msg}");
        assert!(matches!(err, EnvError::Unreadable { .. }));
    }

    #[test]
    fn a_real_file_on_disk_is_read() {
        let dir = std::env::temp_dir().join(format!("skadi-env-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret");
        std::fs::write(&path, "on-disk\n").unwrap();
        let p = path.to_str().unwrap().to_string();
        let got = env_or_file_with("X", env(&[("X_FILE", &p)]), |p: &Path| {
            std::fs::read_to_string(p)
        });
        assert_eq!(got, Ok(Some("on-disk".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn database_url_unchanged_without_a_password_variable() {
        let url = "postgres://skadi:pw@postgres:5432/skadi";
        assert_eq!(
            database_url_with(env(&[(DATABASE_URL_ENV, url)]), files(&[])),
            Ok(Some(url.into()))
        );
        assert_eq!(database_url_with(env(&[]), files(&[])), Ok(None));
    }

    #[test]
    fn database_password_file_is_spliced_into_the_url_encoded() {
        let got = database_url_with(
            env(&[
                (DATABASE_URL_ENV, "postgres://skadi@172.28.0.5:5432/skadi"),
                (
                    "SKADI_DATABASE_PASSWORD_FILE",
                    "/run/secrets/postgres_password",
                ),
            ]),
            files(&[("/run/secrets/postgres_password", "p@ss:w/rd\n")]),
        );
        assert_eq!(
            got,
            Ok(Some(
                "postgres://skadi:p%40ss%3Aw%2Frd@172.28.0.5:5432/skadi".into()
            ))
        );
    }

    #[test]
    fn database_url_file_is_honoured() {
        let got = database_url_with(
            env(&[("SKADI_DATABASE_URL_FILE", "/u")]),
            files(&[("/u", "postgres://a:b@h/d\n")]),
        );
        assert_eq!(got, Ok(Some("postgres://a:b@h/d".into())));
    }

    #[test]
    fn database_password_conflicts_are_errors() {
        // A password in both places.
        let e = database_url_with(
            env(&[
                (DATABASE_URL_ENV, "postgres://skadi:inline@h/d"),
                (DATABASE_PASSWORD_ENV, "other"),
            ]),
            files(&[]),
        )
        .unwrap_err();
        assert!(e.to_string().contains("already holds a password"), "{e}");
        assert!(!e.to_string().contains("inline") && !e.to_string().contains("other"));
        // No user to attach it to.
        let e = database_url_with(
            env(&[
                (DATABASE_URL_ENV, "postgres://h/d"),
                (DATABASE_PASSWORD_ENV, "pw"),
            ]),
            files(&[]),
        )
        .unwrap_err();
        assert!(e.to_string().contains("names no user"), "{e}");
        // A password with no URL.
        let e = database_url_with(env(&[(DATABASE_PASSWORD_ENV, "pw")]), files(&[])).unwrap_err();
        assert!(matches!(e, EnvError::DatabaseUrl(_)));
    }

    #[test]
    fn url_password_finds_the_inline_password() {
        assert_eq!(url_password("postgres://u:secret@h:5432/d"), Some("secret"));
        assert_eq!(url_password("postgres://u@h/d"), None);
        assert_eq!(url_password("sqlite://./skadi.db"), None);
    }
}
