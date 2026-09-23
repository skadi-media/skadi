//! Steps that run the `skadi` binary against the in-process daemon.
use cucumber::{given, then, when};

use skadi_api::DomainDescriptor;
use skadi_core::MediaKind;

use crate::bdd_support::{World, split_args};

#[given("a running daemon in open mode")]
async fn open_daemon(w: &mut World) {
    w.token = None;
    w.daemon().await;
}

#[given(expr = "a running daemon protected by API token {string}")]
async fn protected_daemon(w: &mut World, token: String) {
    w.token = Some(token);
    w.daemon().await;
}

#[given(expr = "the daemon compiles in the {string} domain")]
async fn compiled_domain(w: &mut World, name: String) {
    let kind = match name.as_str() {
        "television" => MediaKind::Series,
        "audiobooks" => MediaKind::Audiobook,
        _ => MediaKind::Movie,
    };
    w.domains.push(DomainDescriptor { name, kind });
}

#[when(expr = "the operator runs {string}")]
async fn run(w: &mut World, line: String) {
    let args = split_args(&line);
    let args = args.into_iter().skip_while(|a| a == "skadi").collect();
    w.run(args, None, None).await;
}

#[when(expr = "the operator runs {string} with SKADI_API_TOKEN {string}")]
async fn run_with_token(w: &mut World, line: String, token: String) {
    let args = split_args(&line);
    let args = args.into_iter().skip_while(|a| a == "skadi").collect();
    w.run(args, Some(token), None).await;
}

#[when(expr = "the operator runs {string} against a daemon that is not running")]
async fn run_no_daemon(w: &mut World, line: String) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().unwrap().port();
    drop(l);
    let args = split_args(&line);
    let args = args.into_iter().skip_while(|a| a == "skadi").collect();
    w.run(args, None, Some(format!("http://127.0.0.1:{port}")))
        .await;
}

#[then(expr = "the command exits {int}")]
async fn exits(w: &mut World, code: i32) {
    let r = w.last();
    assert_eq!(
        r.code, code,
        "exit code; stdout: {}\nstderr: {}",
        r.stdout, r.stderr
    );
}

#[then("the command succeeds")]
async fn succeeds(w: &mut World) {
    exits(w, 0).await;
}

#[then(expr = "stdout is JSON with field {string} equal to {string}")]
async fn stdout_field(w: &mut World, field: String, value: String) {
    let value = w.expand(&value);
    let r = w.last();
    let mut cur = &r.json;
    for seg in field.split('.') {
        cur = match seg.parse::<usize>() {
            Ok(i) => &cur[i],
            Err(_) => &cur[seg],
        };
    }
    let expected: serde_json::Value =
        serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value.clone()));
    assert_eq!(cur, &expected, "stdout: {}", r.stdout);
}

#[then(expr = "stdout is a JSON array of length {int}")]
async fn stdout_array(w: &mut World, n: usize) {
    let r = w.last();
    assert_eq!(
        r.json.as_array().map(Vec::len),
        Some(n),
        "stdout: {}",
        r.stdout
    );
}

#[then(expr = "stdout contains {string}")]
async fn stdout_contains(w: &mut World, needle: String) {
    let r = w.last();
    assert!(r.stdout.contains(&needle), "stdout: {}", r.stdout);
}

#[then(expr = "stderr contains {string}")]
async fn stderr_contains(w: &mut World, needle: String) {
    let r = w.last();
    assert!(r.stderr.contains(&needle), "stderr: {}", r.stderr);
}

#[then("stdout spans more than one line")]
async fn stdout_multiline(w: &mut World) {
    let r = w.last();
    assert!(r.stdout.trim().lines().count() > 1, "stdout: {}", r.stdout);
}

#[then(expr = "the id printed is remembered as {string}")]
async fn remember(w: &mut World, name: String) {
    let id = w.last().json["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in stdout: {}", w.last().stdout))
        .to_string();
    w.ids.insert(name, id);
}
