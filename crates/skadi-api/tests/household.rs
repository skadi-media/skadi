//! Household members (SKADI-T-0611): the admin is created from `api_token`,
//! members get their own tokens, `/me` says who you are, admin-only routes
//! refuse everyone else, and a revoked token stops working.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config};
use skadi_testsupport::TestDb;

fn config() -> Config {
    Config {
        database_url: "sqlite://:memory:".into(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        bearer_token: Some("operator-token".into()),
    }
}

async fn call(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = skadi_api::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn admin_is_created_from_api_token_and_members_get_their_own() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    state.refresh_members().await; // idempotent: still one admin

    let (st, me) = call(&state, "GET", "/api/v1/me", Some("operator-token"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(me["role"], "admin");

    let (st, members) = call(
        &state,
        "GET",
        "/api/v1/members",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        members.as_array().unwrap().len(),
        1,
        "exactly one admin after two refreshes"
    );
    assert!(
        members[0].get("token").is_none(),
        "tokens never appear in listings"
    );

    let (st, created) = call(
        &state,
        "POST",
        "/api/v1/members",
        Some("operator-token"),
        Some(serde_json::json!({"name": "Sam", "role": "kid", "policy": {"max_rating": {"movie": "PG"}}})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{created}");
    let kid_token = created["token"].as_str().unwrap().to_string();
    let kid_id = created["member"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["member"]["policy"]["max_rating"]["movie"], "PG");

    let (st, me) = call(&state, "GET", "/api/v1/me", Some(&kid_token), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(me["role"], "kid");
    assert_eq!(me["name"], "Sam");

    // Using the token records last_seen_at (the page's "seen <date>").
    let (_, members) = call(
        &state,
        "GET",
        "/api/v1/members",
        Some("operator-token"),
        None,
    )
    .await;
    let kid_row = members
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == kid_id.as_str())
        .unwrap();
    assert!(
        kid_row["last_seen_at"].is_string(),
        "kid was seen: {kid_row}"
    );

    // The kid cannot manage members.
    let (st, body) = call(&state, "GET", "/api/v1/members", Some(&kid_token), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(body["message"], "not allowed on this account");

    // Re-issuing invalidates the old token.
    let (st, reissued) = call(
        &state,
        "POST",
        &format!("/api/v1/members/{kid_id}/token"),
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let new_token = reissued["token"].as_str().unwrap().to_string();
    assert_ne!(new_token, kid_token);
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&kid_token), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&new_token), None).await;
    assert_eq!(st, StatusCode::OK);

    // Pairing as the member carries their token; the admin cannot be deleted; revoking works.
    let (st, qr) = call(
        &state,
        "GET",
        &format!("/api/v1/pair/app?member={kid_id}"),
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(qr["url"].as_str().unwrap().contains(&new_token));
    let (st, _) = call(
        &state,
        "DELETE",
        "/api/v1/members/admin",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &state,
        "DELETE",
        &format!("/api/v1/members/{kid_id}"),
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&new_token), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

/// SKADI-T-0612: non-admins reach only the read/play surface. The gate answers
/// before any domain router, so a controller route is a 403 even when the
/// domain that owns it is not mounted here.
#[tokio::test]
async fn non_admin_tokens_are_refused_off_the_read_play_surface() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let (st, created) = call(
        &state,
        "POST",
        "/api/v1/members",
        Some("operator-token"),
        Some(serde_json::json!({"name": "Sam", "role": "kid"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{created}");
    let kid = created["token"].as_str().unwrap().to_string();
    let (st, created) = call(
        &state,
        "POST",
        "/api/v1/members",
        Some("operator-token"),
        Some(serde_json::json!({"name": "Jamie", "role": "member"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{created}");
    let member = created["token"].as_str().unwrap().to_string();

    // Domain routes (POST /movies, …/releases, …) are not mounted in this
    // harness and unknown paths are a 404 ahead of the auth layer by design;
    // `path_allowed`'s unit test covers those, this covers the live layer.
    for (method, path) in [
        ("GET", "/api/v1/wanted"),
        ("GET", "/api/v1/downloads"),
        ("GET", "/api/v1/settings/profiles"),
        ("GET", "/api/v1/pair/app"),
        ("POST", "/api/v1/members"),
    ] {
        for tok in [&kid, &member] {
            let (st, body) = call(&state, method, path, Some(tok), None).await;
            assert_eq!(
                st,
                StatusCode::FORBIDDEN,
                "{method} {path} for a non-admin: {body}"
            );
            assert_eq!(body["message"], "not allowed on this account");
        }
    }
    // The read surface is reachable (404 here only because no domain is mounted).
    for path in [
        "/api/v1/movies/abc",
        "/api/v1/series/abc",
        "/api/v1/books/abc",
    ] {
        let (st, _) = call(&state, "GET", path, Some(&kid), None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path} passes the gate");
    }
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&kid), None).await;
    assert_eq!(st, StatusCode::OK);
    // The operator is untouched.
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/members",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

/// SKADI-T-0620: a username and password become the device token everything
/// else already understands, and a failure tells an attacker nothing.
#[tokio::test]
async fn logging_in_issues_a_device_token_and_failures_are_indistinguishable() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;

    let (st, created) = call(
        &state,
        "POST",
        "/api/v1/members",
        Some("operator-token"),
        Some(serde_json::json!({"name": "Robin", "role": "kid", "password": "kaiju-fan-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{created}");
    assert_eq!(created["member"]["username"], "Robin");
    assert_eq!(created["member"]["has_password"], true);

    // The display name is the login name, and case does not matter.
    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "robin", "password": "kaiju-fan-1", "label": "Pixel"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let token = body["token"].as_str().unwrap().to_string();
    assert_eq!(body["member"]["role"], "kid");

    // The issued token is a real credential for the rest of the API.
    let (st, me) = call(&state, "GET", "/api/v1/me", Some(&token), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(me["name"], "Robin");

    // Wrong password and unknown username answer identically.
    let (st_wrong, wrong) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "robin", "password": "nope"})),
    )
    .await;
    let (st_unknown, unknown) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "nobody-here", "password": "nope"})),
    )
    .await;
    assert_eq!(st_wrong, StatusCode::UNAUTHORIZED);
    assert_eq!(st_unknown, StatusCode::UNAUTHORIZED);
    assert_eq!(
        wrong, unknown,
        "a failure must not say which half was wrong"
    );

    // A member with no password cannot log in, and says no more than that.
    let (st, sam) = call(
        &state,
        "POST",
        "/api/v1/members",
        Some("operator-token"),
        Some(serde_json::json!({"name": "Sam", "role": "member"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{sam}");
    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "Sam", "password": "anything"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(body, wrong);
}

/// The devices are separately revocable, which is what makes "sign out all"
/// and the leaked-password remedy mean anything (SKADI-T-0619/0620).
#[tokio::test]
async fn devices_sign_out_one_at_a_time_or_all_at_once() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/members",
        Some("operator-token"),
        Some(serde_json::json!({"name": "Robin", "role": "kid", "password": "kaiju-fan-1"})),
    )
    .await;
    let id = created["member"]["id"].as_str().unwrap().to_string();

    let login = |label: &'static str| {
        let state = state.clone();
        async move {
            let (_, b) = call(
                &state,
                "POST",
                "/api/v1/auth/login",
                None,
                Some(serde_json::json!({"username": "Robin", "password": "kaiju-fan-1", "label": label})),
            )
            .await;
            b["token"].as_str().unwrap().to_string()
        }
    };
    let phone = login("Pixel").await;
    let tablet = login("Tablet").await;
    assert_ne!(phone, tablet);

    // Logging one out leaves the other signed in.
    let (st, _) = call(&state, "POST", "/api/v1/auth/logout", Some(&phone), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&phone), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "the phone is out");
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&tablet), None).await;
    assert_eq!(st, StatusCode::OK, "the tablet is not");

    // The operator's sign-out-all empties it.
    let (st, view) = call(
        &state,
        "POST",
        &format!("/api/v1/members/{id}/signout"),
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{view}");
    assert_eq!(view["devices"], 0);
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&tablet), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

/// The operator setting a password is the remedy for one that leaked, so it
/// signs every device out; a member changing their own keeps the device in
/// their hand (SKADI-I-0061).
#[tokio::test]
async fn a_password_change_signs_the_right_devices_out() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/members",
        Some("operator-token"),
        Some(serde_json::json!({"name": "Robin", "role": "kid", "password": "kaiju-fan-1"})),
    )
    .await;
    let id = created["member"]["id"].as_str().unwrap().to_string();
    let login = |pw: &'static str| {
        let state = state.clone();
        async move {
            let (st, b) = call(
                &state,
                "POST",
                "/api/v1/auth/login",
                None,
                Some(serde_json::json!({"username": "Robin", "password": pw})),
            )
            .await;
            (st, b["token"].as_str().unwrap_or_default().to_string())
        }
    };
    let (_, phone) = login("kaiju-fan-1").await;
    let (_, tablet) = login("kaiju-fan-1").await;

    // Self-service: the caller stays in, the other device goes.
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/auth/password",
        Some(&phone),
        Some(serde_json::json!({"current": "kaiju-fan-1", "new": "godzilla-2026"})),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&phone), None).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "the device in your hand stays signed in"
    );
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&tablet), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "the other one does not");

    // The old password is gone, the new one works.
    let (st, _) = login("kaiju-fan-1").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, fresh) = login("godzilla-2026").await;
    assert_eq!(st, StatusCode::OK);

    // A wrong current password changes nothing.
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/auth/password",
        Some(&fresh),
        Some(serde_json::json!({"current": "guessing", "new": "something-else"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Operator setting one signs everything out, because that is the remedy.
    let (st, _) = call(
        &state,
        "PATCH",
        &format!("/api/v1/members/{id}"),
        Some("operator-token"),
        Some(serde_json::json!({"password": "operator-reset-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&state, "GET", "/api/v1/me", Some(&fresh), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

/// Two people in one household cannot answer to the same name.
#[tokio::test]
async fn usernames_are_unique_across_the_household() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let make = |name: &'static str| {
        let state = state.clone();
        async move {
            call(
                &state,
                "POST",
                "/api/v1/members",
                Some("operator-token"),
                Some(serde_json::json!({"name": name, "role": "kid"})),
            )
            .await
        }
    };
    let (st, _) = make("Robin").await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, body) = make("robin").await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "case must not buy a second seat: {body}"
    );
}

/// The deploy that adds a login page must not lock the operator out of their
/// own console: they have always authenticated with `api_token` and have no
/// password until they set one (SKADI-I-0061).
#[tokio::test]
async fn the_operator_can_sign_in_with_the_api_token_until_they_set_a_password() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;

    // Migration gives the admin a login name without anyone doing anything.
    let (_, members) = call(
        &state,
        "GET",
        "/api/v1/members",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(members[0]["username"], "Operator");
    assert_eq!(members[0]["has_password"], false);

    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "Operator", "password": "operator-token"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["member"]["role"], "admin");
    let token = body["token"].as_str().unwrap().to_string();
    let (st, _) = call(&state, "GET", "/api/v1/members", Some(&token), None).await;
    assert_eq!(st, StatusCode::OK, "and it is a real admin credential");

    // Anything else is still refused.
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "Operator", "password": "guessing"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Once a real password exists the bootstrap is gone.
    let (st, _) = call(
        &state,
        "PATCH",
        "/api/v1/members/admin",
        Some("operator-token"),
        Some(serde_json::json!({"password": "a-real-one"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "Operator", "password": "operator-token"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNAUTHORIZED,
        "the token stops being a password"
    );
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "Operator", "password": "a-real-one"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // And `api_token` never stopped being the machine credential the CLI uses.
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/members",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

/// Renaming an account renames what it signs in as, because the design says
/// they are the same thing. Without this the rename silently half-applied and
/// looked like it had not worked (operator hit this on 2026-09-22).
#[tokio::test]
async fn renaming_a_member_renames_what_they_sign_in_as() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let (_, created) = call(
        &state,
        "POST",
        "/api/v1/members",
        Some("operator-token"),
        Some(serde_json::json!({"name": "Robin", "role": "kid", "password": "kaiju-fan-1"})),
    )
    .await;
    let id = created["member"]["id"].as_str().unwrap().to_string();

    let (st, view) = call(
        &state,
        "PATCH",
        &format!("/api/v1/members/{id}"),
        Some("operator-token"),
        Some(serde_json::json!({"name": "Robbie"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(view["name"], "Robbie");
    assert_eq!(
        view["username"], "Robbie",
        "the login name follows the rename"
    );

    // And the new name is what actually signs in.
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(serde_json::json!({"username": "Robbie", "password": "kaiju-fan-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // A username deliberately set apart from the name is NOT clobbered by a
    // later rename — only one that was tracking follows.
    let (st, _) = call(
        &state,
        "PATCH",
        &format!("/api/v1/members/{id}"),
        Some("operator-token"),
        Some(serde_json::json!({"username": "mat-the-kaiju-fan"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, view) = call(
        &state,
        "PATCH",
        &format!("/api/v1/members/{id}"),
        Some("operator-token"),
        Some(serde_json::json!({"name": "Robin Cauthon"})),
    )
    .await;
    assert_eq!(view["name"], "Robin Cauthon");
    assert_eq!(
        view["username"], "mat-the-kaiju-fan",
        "a deliberate one stands"
    );
}

/// SKADI-T-0680: forcing a health-check run is the admin's. Every other role is
/// refused by the household gate (`path_allowed`), which the admin skips — so
/// this test signs in as each non-admin role, not as the operator.
#[tokio::test]
async fn only_the_admin_can_force_a_health_check_run() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let mut tokens = Vec::new();
    for (name, role) in [("Kim", "kid"), ("Jo", "member"), ("Cam", "contributor")] {
        let (st, created) = call(
            &state,
            "POST",
            "/api/v1/members",
            Some("operator-token"),
            Some(serde_json::json!({"name": name, "role": role})),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{created}");
        tokens.push((role, created["token"].as_str().unwrap().to_string()));
    }
    for (role, tok) in &tokens {
        for uri in [
            "/api/v1/health/checks/run",
            "/api/v1/health/checks/run?id=database",
        ] {
            let (st, body) = call(&state, "POST", uri, Some(tok), None).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "POST {uri} as {role}: {body}");
            assert_eq!(body["message"], "not allowed on this account");
        }
    }
    // Nothing ran: the cache still lists the database as pending.
    let (st, checks) = call(
        &state,
        "GET",
        "/api/v1/health/checks",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let db_check = |checks: &serde_json::Value| {
        checks
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "database")
            .cloned()
            .unwrap()
    };
    assert!(db_check(&checks)["checked_at"].is_null(), "{checks}");

    let (st, checks) = call(
        &state,
        "POST",
        "/api/v1/health/checks/run",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{checks}");
    assert_eq!(db_check(&checks)["severity"], "ok");
    assert!(db_check(&checks)["checked_at"].is_string());

    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/health/checks/run?id=no-such-check",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// SKADI-T-0684: the web System page reads these four endpoints. The page is
/// drawn for the admin only, and the API must agree: every other role gets a
/// 403 on each (SKADI-T-0639, "UI and API agree"). Signed in as each non-admin
/// role, because the admin skips `path_allowed`.
#[tokio::test]
async fn the_system_page_endpoints_are_the_admins_alone() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let uris = [
        "/api/v1/system/status",
        "/api/v1/system/task",
        "/api/v1/log?pageSize=50",
        "/api/v1/health/checks",
    ];
    for (name, role) in [("Kim", "kid"), ("Jo", "member"), ("Cam", "contributor")] {
        let (st, created) = call(
            &state,
            "POST",
            "/api/v1/members",
            Some("operator-token"),
            Some(serde_json::json!({"name": name, "role": role})),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{created}");
        let tok = created["token"].as_str().unwrap().to_string();
        for uri in uris {
            let (st, body) = call(&state, "GET", uri, Some(&tok), None).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "GET {uri} as {role}: {body}");
            assert_eq!(body["message"], "not allowed on this account");
        }
    }
    for uri in uris {
        let (st, body) = call(&state, "GET", uri, Some("operator-token"), None).await;
        assert_eq!(st, StatusCode::OK, "GET {uri} as admin: {body}");
    }
    // The status header shows the build commit: a git sha here (the tests
    // build from a checkout), never an empty string.
    let (_, status) = call(
        &state,
        "GET",
        "/api/v1/system/status",
        Some("operator-token"),
        None,
    )
    .await;
    let commit = status["commit"].as_str().expect("commit is a string");
    assert!(!commit.is_empty(), "{status}");
    assert_eq!(commit, skadi_api::diagnostics::BUILD_COMMIT);
}

/// SKADI-T-0689: the manual import the Downloads page offers is the admin's
/// alone. A kid, member or contributor is refused both the preview and the
/// import (and the Downloads list itself) at the gate; the admin passes it.
#[tokio::test]
async fn only_the_admin_can_preview_or_run_a_manual_import() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let body = serde_json::json!({
        "path": "/nowhere/Movie.mkv",
        "kind": "movie",
        "acquirable_ref": "ed-1",
    });
    let routes = [
        ("POST", "/api/v1/downloads/import/preview"),
        ("POST", "/api/v1/downloads/import"),
        ("GET", "/api/v1/downloads"),
    ];
    for (name, role) in [("Kai", "kid"), ("Mo", "member"), ("Cy", "contributor")] {
        let (st, created) = call(
            &state,
            "POST",
            "/api/v1/members",
            Some("operator-token"),
            Some(serde_json::json!({"name": name, "role": role})),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{created}");
        let tok = created["token"].as_str().unwrap().to_string();
        for (method, uri) in routes {
            let (st, got) = call(&state, method, uri, Some(&tok), Some(body.clone())).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{method} {uri} as {role}: {got}");
            assert_eq!(got["message"], "not allowed on this account");
        }
    }
    // The admin gets past the gate: the handler answers (no domain is mounted
    // here, so the kind is unknown: a 400 from the handler, not a 403).
    for (method, uri) in routes {
        let (st, got) = call(
            &state,
            method,
            uri,
            Some("operator-token"),
            Some(body.clone()),
        )
        .await;
        assert_ne!(st, StatusCode::FORBIDDEN, "{method} {uri} as admin: {got}");
        assert_ne!(
            st,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} as admin: {got}"
        );
    }
}

/// The Activity row actions (SKADI-T-0691) are the admin's alone: the queue
/// itself, the new Retry route, and every existing endpoint the row composes
/// (Pause / Resume / Remove on `/downloads/{id}`, the release key from
/// `/decisions`, `POST /blocklist`). A kid, member or contributor is refused
/// each at the gate (Admin short-circuits `path_allowed`, so only these roles
/// can show a hole); the admin gets past it.
#[tokio::test]
async fn only_the_admin_can_act_on_an_activity_row() {
    let db = TestDb::new_store_only().await;
    let state = AppState::new_full(config(), Some(db.store.clone()), vec![], vec![]);
    state.refresh_members().await;
    let body = serde_json::json!({"release_key": "btih:0691", "title": "x"});
    let routes = [
        ("GET", "/api/v1/activity"),
        ("POST", "/api/v1/activity/ed-0691/retry"),
        ("POST", "/api/v1/downloads/dl-0691/pause"),
        ("POST", "/api/v1/downloads/dl-0691/resume"),
        ("DELETE", "/api/v1/downloads/dl-0691?delete_data=true"),
        ("GET", "/api/v1/decisions?acquirable=ed-0691"),
        ("POST", "/api/v1/blocklist"),
    ];
    for (name, role) in [("Kai", "kid"), ("Mo", "member"), ("Cy", "contributor")] {
        let (st, created) = call(
            &state,
            "POST",
            "/api/v1/members",
            Some("operator-token"),
            Some(serde_json::json!({"name": name, "role": role})),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{created}");
        let tok = created["token"].as_str().unwrap().to_string();
        for (method, uri) in routes {
            let (st, got) = call(&state, method, uri, Some(&tok), Some(body.clone())).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{method} {uri} as {role}: {got}");
            assert_eq!(got["message"], "not allowed on this account");
        }
    }
    // The admin passes the gate. The retry answers 404 (no such run), the
    // others whatever their handler says about the made-up ids, never 403.
    for (method, uri) in routes {
        let (st, got) = call(
            &state,
            method,
            uri,
            Some("operator-token"),
            Some(body.clone()),
        )
        .await;
        assert_ne!(st, StatusCode::FORBIDDEN, "{method} {uri} as admin: {got}");
        assert_ne!(
            st,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} as admin: {got}"
        );
    }
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/activity/ed-0691/retry",
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
