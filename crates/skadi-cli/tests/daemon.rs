//! End-to-end CLI/daemon smoke test (SKADI-T-0056).
//!
//! Spawns the built `skadi run` binary on a tempdir SQLite DB + a free port,
//! waits for `/health`, then drives a couple of `skadi` client subcommands as
//! child processes (the same way a user would) and asserts they succeed against
//! the live daemon. Proves the bin's `run` path (bootstrap + supervisor + serve)
//! and the client path both work over real HTTP.

use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Path to the compiled `skadi` binary (cargo sets CARGO_BIN_EXE_<name>).
fn skadi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_skadi"))
}

/// Grab a free localhost port by binding to :0 and releasing it.
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn daemon_serves_and_cli_talks_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("skadi.db");
    let url = format!("sqlite://{}", db.display());
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let base = format!("http://127.0.0.1:{port}");

    // Spawn `skadi run` in open mode (no token).
    let child = Command::new(skadi_bin())
        .arg("run")
        .env("SKADI_DATABASE_URL", &url)
        .env("SKADI_BIND_ADDR", &bind)
        .env_remove("SKADI_API_TOKEN")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn skadi run");
    let mut daemon = Daemon { child };

    // Poll /health until the server is up (≤30s).
    let deadline = Instant::now() + Duration::from_secs(30);
    let health_url = format!("{base}/api/v1/health");
    let mut up = false;
    while Instant::now() < deadline {
        if let Ok(resp) = reqwest::blocking::get(&health_url)
            && resp.status().is_success()
        {
            up = true;
            break;
        }
        // Surface an early crash instead of waiting the full deadline.
        if let Ok(Some(status)) = daemon.child.try_wait() {
            let mut err = String::new();
            if let Some(mut s) = daemon.child.stderr.take() {
                let _ = s.read_to_string(&mut err);
            }
            panic!("daemon exited early ({status}):\n{err}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(up, "daemon /health never came up");

    // `skadi health` via the binary's client path.
    let out = Command::new(skadi_bin())
        .args(["--url", &base, "health"])
        .output()
        .expect("run skadi health");
    assert!(out.status.success(), "skadi health failed: {out:?}");
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("\"status\":\"ok\""), "health body: {body}");

    // `skadi domain list` should show the movies domain, disabled by default.
    let out = Command::new(skadi_bin())
        .args(["--url", &base, "domain", "list"])
        .output()
        .expect("run skadi domain list");
    assert!(out.status.success(), "domain list failed: {out:?}");
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("movies"), "domain list body: {body}");

    // Enable the movies domain through the CLI, then confirm it reflects.
    let out = Command::new(skadi_bin())
        .args(["--url", &base, "domain", "enable", "movies"])
        .output()
        .expect("run skadi domain enable");
    assert!(out.status.success(), "domain enable failed: {out:?}");

    let out = Command::new(skadi_bin())
        .args(["--url", &base, "domain", "list"])
        .output()
        .expect("run skadi domain list 2");
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        body.contains("\"enabled\":true"),
        "expected movies enabled: {body}"
    );

    // Clean shutdown happens via Daemon::drop.
}
