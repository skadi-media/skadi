//! The role surface table shared with the web client (SKADI-T-0642).
//!
//! `role_surface.tsv` lists (role, method, path, verdict) rows. This test
//! asserts the server's `household::path_allowed` gives each verdict;
//! `skadi-web`'s `surface` tests assert its port gives the same. A change to
//! either table that the other does not follow fails one of the two.
//!
//! To regenerate after a deliberate change to `path_allowed`:
//! `SKADI_BLESS_ROLE_SURFACE=1 cargo test -p skadi-api --test role_surface`,
//! then port the change to `crates/skadi-web/src/surface.rs`.

use axum::http::Method;
use skadi_api::household::{Role, path_allowed};

const ROLES: &[(&str, Role)] = &[
    ("admin", Role::Admin),
    ("member", Role::Member),
    ("contributor", Role::Contributor),
    ("kid", Role::Kid),
];

/// Every arm of `path_allowed`, and the near-misses that have shadowed one
/// before (an id that is really `lookup`, a list without its detail arm).
const CASES: &[(&str, &str)] = &[
    ("GET", "/me"),
    ("GET", "/health"),
    ("GET", "/health/ready"),
    ("GET", "/health/checks"),
    ("POST", "/auth/logout"),
    ("POST", "/auth/password"),
    ("GET", "/movies/lookup"),
    ("GET", "/series/lookup"),
    ("GET", "/books/lookup"),
    ("GET", "/books/search"),
    ("GET", "/wanted"),
    ("GET", "/settings/profiles"),
    ("POST", "/settings/profiles"),
    ("GET", "/settings/indexers"),
    ("GET", "/movies"),
    ("GET", "/movies/"),
    ("HEAD", "/movies"),
    ("GET", "/movies/m1"),
    ("GET", "/movies/m1/editions/e1/video"),
    ("GET", "/movies/m1/editions"),
    ("GET", "/series"),
    ("GET", "/series/s1"),
    ("GET", "/series/s1/episodes/e1/video"),
    ("GET", "/books"),
    ("GET", "/books/b1"),
    ("GET", "/books/b1/files/f1/audio"),
    ("GET", "/books/b1/files/f1/chapters"),
    ("GET", "/library/genres"),
    ("GET", "/domains"),
    ("GET", "/events"),
    ("GET", "/edition-kinds"),
    ("POST", "/edition-kinds"),
    ("GET", "/authors"),
    ("GET", "/authors/a1"),
    ("GET", "/audiobooks/series"),
    ("GET", "/audiobooks/works"),
    ("GET", "/audiobooks/discover"),
    ("GET", "/downloads"),
    ("GET", "/history"),
    ("GET", "/activity"),
    ("GET", "/traces"),
    ("GET", "/root-folders"),
    ("GET", "/watchers"),
    ("POST", "/watchers"),
    ("GET", "/system/status"),
    ("GET", "/members"),
    ("GET", "/config"),
    ("PUT", "/config"),
    ("POST", "/movies"),
    ("POST", "/series"),
    ("POST", "/books"),
    ("POST", "/movies/m1/editions/e1/acquire"),
    ("POST", "/series/s1/episodes/e1/acquire"),
    ("POST", "/books/b1/files/f1/acquire"),
    ("POST", "/movies/bulk"),
    ("DELETE", "/movies/m1"),
    ("PATCH", "/movies/m1"),
    ("GET", "/uploads"),
    ("POST", "/uploads"),
    ("PUT", "/uploads/u1/chunk"),
    ("POST", "/uploads/u1/complete"),
    ("DELETE", "/uploads/u1"),
    ("GET", "/uploads/u1"),
];

fn method(m: &str) -> Method {
    m.parse().expect("method")
}

fn rows() -> String {
    let mut out = String::from(
        "# role\tmethod\tpath\tverdict — generated from household::path_allowed\n\
         # (SKADI-T-0642). Regenerate: see crates/skadi-api/tests/role_surface.rs.\n",
    );
    for (name, role) in ROLES {
        for (m, path) in CASES {
            let v = if path_allowed(*role, &method(m), path) {
                "allow"
            } else {
                "deny"
            };
            out.push_str(&format!("{name}\t{m}\t{path}\t{v}\n"));
        }
    }
    out
}

#[test]
fn the_server_table_agrees_with_the_shared_cases() {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/role_surface.tsv");
    let want = rows();
    if std::env::var_os("SKADI_BLESS_ROLE_SURFACE").is_some() {
        std::fs::write(&file, &want).expect("write the table");
        return;
    }
    let have = std::fs::read_to_string(&file).expect("tests/role_surface.tsv");
    assert_eq!(
        have, want,
        "path_allowed and tests/role_surface.tsv disagree; see this file's header"
    );
}
