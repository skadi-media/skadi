//! The API surface each household role may call, on the client (SKADI-T-0642).
//!
//! A port of `skadi_api::household::path_allowed`, **arm for arm**: this crate
//! is a wasm target and cannot depend on `skadi-api`, so the table is copied,
//! and `crates/skadi-api/tests/role_surface.tsv` holds the cases both copies
//! are tested against. Change one, change the other, and the shared table.
//!
//! This is **not** a security boundary. The server's `household_gate` refuses
//! what it refuses whatever the client does. The table exists so the UI does
//! not *offer* or *fire* what will be refused: every request passes through
//! `api`'s helpers, which consult it and answer [`crate::api::ApiError::not_available`]
//! instead of sending.

/// A household role, as `GET /me` names it (`household::Role`, lowercase).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Admin,
    Member,
    Contributor,
    Kid,
}

impl Role {
    /// The role `GET /me` names. An unknown string is the narrowest role, not
    /// trusted with more (`subnav` has always read roles this way).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "admin" => Role::Admin,
            "member" => Role::Member,
            "contributor" => Role::Contributor,
            _ => Role::Kid,
        }
    }

    fn can_contribute(self) -> bool {
        matches!(self, Role::Admin | Role::Contributor)
    }
}

/// Whether `role` may call `method` on `path` (as the nested API sees it: no
/// `/api/v1`, no query). Mirrors `household::path_allowed`.
#[must_use]
pub fn allowed(role: Role, method: &str, path: &str) -> bool {
    if role == Role::Admin {
        return true;
    }
    if path == "/auth/logout" || path == "/auth/password" {
        return true;
    }
    let seg: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if role.can_contribute() {
        let adding = matches!(seg.as_slice(), ["movies"] | ["series"] | ["books"]);
        let searching = matches!(
            seg.as_slice(),
            ["movies", _, "editions", _, "acquire"]
                | ["series", _, "episodes", _, "acquire"]
                | ["books", _, "files", _, "acquire"]
        );
        if method == "POST" && (adding || searching) {
            return true;
        }
        let uploading = matches!(
            seg.as_slice(),
            ["uploads"] | ["uploads", _, "chunk"] | ["uploads", _, "complete"] | ["uploads", _]
        );
        if uploading {
            return true;
        }
    }
    if method != "GET" && method != "HEAD" {
        return false;
    }
    match seg.as_slice() {
        ["me"] | ["health"] | ["health", "ready"] => true,
        // Before the `["movies", _]` arms: they would take "lookup" as an id.
        ["movies", "lookup"] | ["series", "lookup"] | ["books", "lookup"] | ["books", "search"] => {
            role.can_contribute()
        }
        ["wanted"] => role.can_contribute(),
        ["settings", "profiles"] => role.can_contribute(),
        ["movies"] | ["movies", _] => true,
        ["movies", _, "editions", _, "video"] => true,
        ["series"] | ["series", _] => true,
        ["series", _, "episodes", _, "video"] => true,
        ["books"] | ["books", _] => true,
        ["books", _, "files", _, "audio"] | ["books", _, "files", _, "chapters"] => true,
        ["library", "genres"] => true,
        ["domains"] => true,
        ["edition-kinds"] => true,
        ["authors"]
        | ["authors", _]
        | ["audiobooks", "series"]
        | ["audiobooks", "works"]
        | ["audiobooks", "discover"] => role != Role::Kid,
        _ => false,
    }
}

/// Whether to **offer** a control whose action is `method path` (SKADI-T-0639):
/// only to a role that is known and may make the call. An unresolved role is
/// offered nothing, the rule that stopped the kid's 403s: gate on a known-good
/// role, never on the absence of a bad one.
#[must_use]
pub fn offered(role: Option<&str>, method: &str, path: &str) -> bool {
    role.is_some_and(|r| allowed(Role::parse(r), method, path))
}

/// The nested API path of a request URL: `/api/v1/movies/1?x=y` → `/movies/1`.
#[must_use]
pub fn api_path(url: &str) -> &str {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    no_query
        .strip_prefix(crate::api::API_BASE)
        .unwrap_or(no_query)
}

/// Whether a request may be sent for a role that may not be known yet. An
/// unknown role (before `/me` answers, or open mode before it is asked) sends,
/// as before this module: the server still decides, and suppressing a page's
/// first fetch for an operator whose `/me` is in flight would break the page.
/// Pages that must not fetch while the role is unknown gate on a known role
/// themselves (SKADI-T-0639).
#[must_use]
pub fn may_send(role: Option<&str>, method: &str, url: &str) -> bool {
    role.is_none_or(|r| allowed(Role::parse(r), method, api_path(url)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared table (SKADI-T-0642): the server's `path_allowed` is tested
    /// against the same file, so the two copies agree on every row.
    #[test]
    fn the_client_table_agrees_with_the_shared_cases() {
        let table = include_str!("../../skadi-api/tests/role_surface.tsv");
        let mut rows = 0;
        for line in table
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        {
            let cols: Vec<&str> = line.split('\t').collect();
            let [role, method, path, want] = cols.as_slice() else {
                panic!("bad row: {line:?}");
            };
            let want = match *want {
                "allow" => true,
                "deny" => false,
                other => panic!("bad verdict {other:?} in {line:?}"),
            };
            assert_eq!(
                allowed(Role::parse(role), method, path),
                want,
                "{role} {method} {path}"
            );
            rows += 1;
        }
        assert!(rows > 200, "the table has {rows} rows");
    }

    #[test]
    fn a_url_is_reduced_to_the_nested_path() {
        assert_eq!(api_path("/api/v1/movies/1?x=2"), "/movies/1");
        assert_eq!(api_path("/api/v1/history?acquirable=a#f"), "/history");
        assert_eq!(api_path("/elsewhere"), "/elsewhere");
    }

    #[test]
    fn a_control_is_offered_only_to_a_known_role_that_may_use_it() {
        assert!(!offered(None, "PUT", "/watchers/book/b1"));
        assert!(offered(Some("admin"), "PUT", "/watchers/book/b1"));
        assert!(!offered(Some("contributor"), "PUT", "/watchers/book/b1"));
        assert!(offered(Some("contributor"), "POST", "/books"));
        assert!(!offered(Some("member"), "POST", "/books"));
        assert!(!offered(Some("kid"), "POST", "/books"));
    }

    #[test]
    fn an_unknown_role_sends_and_a_known_one_is_gated() {
        assert!(may_send(None, "GET", "/api/v1/downloads"));
        assert!(may_send(Some("admin"), "GET", "/api/v1/downloads"));
        assert!(!may_send(Some("member"), "GET", "/api/v1/downloads"));
        assert!(!may_send(Some("kid"), "GET", "/api/v1/authors?page=1"));
        assert!(may_send(Some("member"), "GET", "/api/v1/authors?page=1"));
        assert!(!may_send(Some("weird"), "GET", "/api/v1/authors"));
    }
}
