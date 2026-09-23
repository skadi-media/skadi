//! Shared BDD world + step modules for `skadi-api` (SKADI-I-0057 pass P6).
//!
//! Every scenario gets its own isolated store (`skadi_testsupport::TestDb`,
//! SQLite tempfile by default) and drives the production router
//! (`skadi_api::router`) in-process with `tower::ServiceExt::oneshot` — no
//! network, no daemon. Domain routes are out of scope here (they belong to the
//! domain crates); a [`FakeLibrary`] stands in for a domain's `LibraryProvider`
//! so `/library`, `/wanted` and `/root-folders/{id}/unmapped` can be exercised.
pub mod steps;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

use skadi_api::{AppState, Config, DomainDescriptor, LibraryItemDto, LibraryProvider, Supervisor};
use skadi_core::MediaKind;
use skadi_store::Store;
use skadi_testsupport::TestDb;

/// `TestDb` has no `Debug`; the world needs one.
pub struct Db(pub TestDb);
impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Db({})", self.0.backend_name())
    }
}

/// `AppState` has no `Debug` either.
pub struct Api(pub Arc<AppState>);
impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AppState")
    }
}

/// `Supervisor` wrapper for the same reason.
pub struct Sup(pub Supervisor);
impl std::fmt::Debug for Sup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Supervisor")
    }
}

/// One captured HTTP response.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub text: String,
    pub json: serde_json::Value,
}

/// A stand-in domain library contributed to the unified `/library` view.
#[derive(Debug, Clone)]
pub struct FakeLibrary {
    pub domain: String,
    pub kind: MediaKind,
    pub items: Vec<LibraryItemDto>,
    pub occupied: Vec<PathBuf>,
}

#[async_trait]
impl LibraryProvider for FakeLibrary {
    fn domain(&self) -> &str {
        &self.domain
    }
    fn kind(&self) -> MediaKind {
        self.kind
    }
    async fn items(&self, monitored: Option<bool>) -> skadi_core::Result<Vec<LibraryItemDto>> {
        Ok(self
            .items
            .iter()
            .filter(|i| monitored.is_none_or(|m| i.monitored == m))
            .cloned()
            .collect())
    }
    async fn occupied_folders(&self) -> skadi_core::Result<Vec<PathBuf>> {
        Ok(self.occupied.clone())
    }
}

/// A provider reloader that only counts how often the supervisor applied a
/// rebuilt provider set.
#[derive(Debug, Default)]
pub struct CountingReloader {
    pub applied: AtomicUsize,
}

#[async_trait]
impl skadi_api::ProviderReloader for CountingReloader {
    async fn apply(&self, _set: skadi_api::ProviderSet) -> skadi_core::Result<()> {
        self.applied.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug, Default, cucumber::World)]
pub struct World {
    /// Free-form scratch for simple scenarios; prefer typed fields for real ones.
    pub notes: Vec<String>,
    pub db: Option<Db>,
    pub api: Option<Api>,
    /// The bearer token the daemon is configured with (`None` = open mode).
    pub token: Option<String>,
    /// The `Authorization` header value the client presents, if any.
    pub presented: Option<String>,
    /// Extra request headers the client presents.
    pub extra_headers: Vec<(String, String)>,
    pub domains: Vec<DomainDescriptor>,
    pub libraries: Vec<FakeLibrary>,
    pub reply: Option<Reply>,
    /// Remembered ids by name (`<name>` in a path expands to the id).
    pub ids: HashMap<String, String>,
    pub tmp: Option<tempfile::TempDir>,
    pub reloaders: Vec<Arc<CountingReloader>>,
    pub supervisor: Option<Sup>,
    // ---- foundation (pass P5) ----
    /// The compiled-in probe domain module(s) the supervisor/bootstrap sees.
    pub modules: Vec<Arc<steps::foundation::ProbeModule>>,
    /// Whether a reloader that always fails is registered with the supervisor.
    pub failing_reloader: bool,
    /// Outcome of the last supervisor tick.
    pub tick_err: Option<String>,
    /// Outcome of the last `build_providers`: `Ok((indexers, downloaders, notifiers))`.
    pub build: Option<Result<(usize, usize, usize), String>>,
    /// A second store opened against the scenario database (a "restart").
    pub alt_store: Option<AltStore>,
    /// Subscription to the hunter sweep trigger.
    pub sweep_rx: Option<tokio::sync::watch::Receiver<u64>>,
    /// Env vars this scenario set, restored afterwards (`@serial` only).
    pub env_touched: Vec<(String, Option<String>)>,
}

/// `Store` has no `Debug`.
pub struct AltStore(pub Store);
impl std::fmt::Debug for AltStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AltStore")
    }
}

impl World {
    /// The scenario's isolated store (created on first use).
    pub async fn store(&mut self) -> Store {
        if self.db.is_none() {
            self.db = Some(Db(TestDb::new_store_only().await));
        }
        self.db.as_ref().expect("db").0.store.clone()
    }

    pub fn tmp(&mut self) -> PathBuf {
        if self.tmp.is_none() {
            self.tmp = Some(tempfile::tempdir().expect("tempdir"));
        }
        self.tmp.as_ref().unwrap().path().to_path_buf()
    }

    /// Forget the built `AppState` so the next request rebuilds it from the
    /// current token / domains / libraries (the store is kept).
    pub fn reset_api(&mut self) {
        self.api = None;
    }

    /// The daemon's `AppState`, built lazily from the world's configuration.
    pub async fn api(&mut self) -> Arc<AppState> {
        if self.api.is_none() {
            let store = self.store().await;
            let url = self.db.as_ref().expect("db").0.url().to_string();
            let config = Config {
                database_url: url,
                bind_addr: "127.0.0.1:0".parse().unwrap(),
                bearer_token: self.token.clone(),
            };
            let libraries: Vec<Arc<dyn LibraryProvider>> = self
                .libraries
                .iter()
                .cloned()
                .map(|l| Arc::new(l) as Arc<dyn LibraryProvider>)
                .collect();
            let state = AppState::new_full(config, Some(store), self.domains.clone(), libraries);
            self.api = Some(Api(state));
        }
        self.api.as_ref().expect("api").0.clone()
    }

    /// Replace `<name>` placeholders in a path with remembered ids.
    pub fn expand(&self, path: &str) -> String {
        let mut out = path.to_string();
        for (name, id) in &self.ids {
            out = out.replace(&format!("<{name}>"), id);
        }
        out
    }

    /// Send one request through the production router and capture the reply.
    pub async fn call(&mut self, method: &str, path: &str, body: Option<(String, &str)>) -> Reply {
        let path = self.expand(path);
        let state = self.api().await;
        let mut builder = Request::builder().method(method).uri(path.as_str());
        if let Some(auth) = &self.presented {
            builder = builder.header("authorization", auth.as_str());
        }
        for (k, v) in &self.extra_headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let request = match body {
            Some((content, ctype)) => builder
                .header("content-type", ctype)
                .body(Body::from(content))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let res = skadi_api::router(state)
            .oneshot(request)
            .await
            .expect("router never fails");
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        let reply = Reply {
            status,
            headers,
            text,
            json,
        };
        self.reply = Some(reply.clone());
        reply
    }

    pub fn reply(&self) -> &Reply {
        self.reply.as_ref().expect("no request has been made yet")
    }

    /// Resolve a dotted path (`items.0.title`) into the last JSON reply.
    pub fn field(&self, path: &str) -> Option<&serde_json::Value> {
        let mut cur = &self.reply().json;
        for seg in path.split('.') {
            cur = match seg.parse::<usize>() {
                Ok(i) => cur.get(i)?,
                Err(_) => cur.get(seg)?,
            };
        }
        Some(cur)
    }
}

/// Media kind for a compiled-in domain name.
pub fn kind_for(domain: &str) -> MediaKind {
    match domain {
        "television" => MediaKind::Series,
        "audiobooks" => MediaKind::Audiobook,
        _ => MediaKind::Movie,
    }
}
