//! Shared BDD world for `skadi-cli` (SKADI-I-0057 pass P6).
//!
//! Each scenario boots the real `skadi-api` router **in-process** on a free
//! loopback port over a scratch SQLite store (no domain routes — the CLI's
//! client path is what is under test), then runs the compiled `skadi` binary
//! exactly as an operator would and captures its exit code / stdout / stderr.
pub mod steps;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use skadi_api::{AppState, Config, DomainDescriptor};
use skadi_core::MediaKind;
use skadi_store::Store;

/// `AppState` has no `Debug`.
pub struct Api(pub Arc<AppState>);
impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AppState")
    }
}

/// One CLI invocation's outcome.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub json: serde_json::Value,
}

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub tmp: Option<tempfile::TempDir>,
    /// The in-process daemon, once started.
    pub api: Option<Api>,
    pub base_url: Option<String>,
    /// Token the daemon requires (`None` = open mode).
    pub token: Option<String>,
    pub domains: Vec<DomainDescriptor>,
    pub last: Option<Run>,
    pub ids: HashMap<String, String>,
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(api) = &self.api {
            api.0.cancel.cancel();
        }
    }
}

impl World {
    /// Boot the daemon (idempotent) and return its base URL.
    pub async fn daemon(&mut self) -> String {
        if let Some(u) = &self.base_url {
            return u.clone();
        }
        let dir = self
            .tmp
            .get_or_insert_with(|| tempfile::tempdir().expect("tempdir"));
        let db_url = format!("sqlite://{}", dir.path().join("skadi.db").display());
        let store = Store::connect(&db_url).expect("connect store");
        store.run_migrations().await.expect("migrations");

        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().unwrap().port()
        };
        let config = Config {
            database_url: db_url,
            bind_addr: format!("127.0.0.1:{port}").parse().unwrap(),
            bearer_token: self.token.clone(),
        };
        if self.domains.is_empty() {
            self.domains.push(DomainDescriptor {
                name: "movies".into(),
                kind: MediaKind::Movie,
            });
        }
        let state = AppState::new_with_domains(config, Some(store), self.domains.clone());
        tokio::spawn(skadi_api::serve(state.clone(), Vec::new()));
        self.api = Some(Api(state));
        let base = format!("http://127.0.0.1:{port}");

        // Wait for /health (≤10s).
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(r) = reqwest::get(format!("{base}/api/v1/health")).await
                && r.status().is_success()
            {
                break;
            }
            assert!(Instant::now() < deadline, "in-process daemon never came up");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.base_url = Some(base.clone());
        base
    }

    /// Run the `skadi` binary with `args` (already split) against the daemon.
    pub async fn run(
        &mut self,
        args: Vec<String>,
        token: Option<String>,
        url: Option<String>,
    ) -> Run {
        let base = match url {
            Some(u) => u,
            None => self.daemon().await,
        };
        let args: Vec<String> = args.into_iter().map(|a| self.expand(&a)).collect();
        let out = tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_skadi"));
            cmd.arg("--url")
                .arg(&base)
                .args(&args)
                .env_remove("SKADI_API_TOKEN")
                .env_remove("SKADI_API_URL");
            if let Some(t) = token {
                cmd.env("SKADI_API_TOKEN", t);
            }
            cmd.output().expect("run skadi binary")
        })
        .await
        .expect("spawn_blocking");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let run = Run {
            code: out.status.code().unwrap_or(-1),
            json: serde_json::from_str(stdout.trim()).unwrap_or(serde_json::Value::Null),
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        self.last = Some(run.clone());
        run
    }

    pub fn last(&self) -> &Run {
        self.last.as_ref().expect("no command has run yet")
    }

    pub fn expand(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (k, v) in &self.ids {
            out = out.replace(&format!("<{k}>"), v);
        }
        out
    }
}

/// Split a command line the way a shell would for the simple cases the
/// features use: whitespace-separated, with single- or double-quoted args.
pub fn split_args(line: &str) -> Vec<String> {
    // Cucumber keeps `\"` escapes literal inside `{string}`; the features write JSON.
    let line = line.replace("\\\"", "\"");
    let line = line.as_str();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has_token = true;
            }
            (None, c) if c.is_whitespace() => {
                if has_token || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            (None, c) => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token || !cur.is_empty() {
        out.push(cur);
    }
    out
}
