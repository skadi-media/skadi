//! Provider factory (SKADI-T-0060): stored settings + sealed credentials →
//! live provider instances.
//!
//! [`build_providers`] reads the provider settings rows (`indexers`,
//! `downloaders`, `notifiers`), deserializes each `body` into its typed config
//! (`IndexerConfig` / `DownloaderConfig` / `NotifierConfig`), pulls the row's
//! secret from the encrypted [`CredentialRepo`](skadi_store::CredentialRepo),
//! and builds the concrete provider.
//!
//! **One bad row never sinks the set**: a row that fails to deserialize, lacks
//! a required secret, or fails validation is logged (`warn!`) and skipped — the
//! factory returns whatever built successfully. The daemon should run with the
//! providers that work rather than refuse to start over one typo.
//!
//! The factory is pure construction: it takes `&Store`, returns a
//! [`ProviderSet`]. Installing the set into the hunter's services registry is
//! the reload loop's job (SKADI-T-0062).

use std::sync::Arc;
use std::time::Duration;

use skadi_core::{DownloaderId, IndexerId, NotifierId, Result};
use skadi_downloaders::{DbDownloader, Downloader, DownloaderConfig};
use skadi_http::HttpClient;
use skadi_indexers::definitions::DefinitionStore;
use skadi_indexers::{Indexer, IndexerConfig};
use skadi_notify::{Notifier, NotifierConfig};
use skadi_store::{CredentialRepo, DownloadJobRepo, SettingsRepo, Store};

/// Default request timeout for provider HTTP clients.
const PROVIDER_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// The live providers built from stored configuration.
#[derive(Default)]
pub struct ProviderSet {
    pub indexers: Vec<Arc<dyn Indexer>>,
    pub downloaders: Vec<Arc<dyn Downloader>>,
    pub notifiers: Vec<Arc<dyn Notifier>>,
}

impl ProviderSet {
    /// Total number of live providers (diagnostics).
    #[must_use]
    pub fn len(&self) -> usize {
        self.indexers.len() + self.downloaders.len() + self.notifiers.len()
    }

    /// True when no providers are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Parse a settings row id (UUID string) into a typed provider id, so a
/// provider's identity is stable across rebuilds and matches its settings row.
fn parse_id<T: From<uuid::Uuid>>(kind: &str, id: &str) -> Option<T> {
    match uuid::Uuid::parse_str(id) {
        Ok(u) => Some(T::from(u)),
        Err(e) => {
            tracing::warn!(kind, id, error = %e, "settings id is not a UUID; skipping provider");
            None
        }
    }
}

/// Build the full provider set from the store. Rows are processed in the
/// repo's listing order (by id), so the set is deterministic across rebuilds.
/// The configured cardigann definitions directory (falls back to the default if
/// config can't be read).
async fn cardigann_definitions_dir(store: &Store) -> String {
    match crate::load_config_view(store).await {
        Ok(view) => view
            .get_string("cardigann_definitions_dir")
            .unwrap_or_else(|_| "./definitions".into()),
        Err(_) => "./definitions".into(),
    }
}

/// The configured FlareSolverr endpoint (empty/unset ⇒ `None`, direct only).
async fn flaresolverr_url(store: &Store) -> Option<String> {
    config_str(store, "flaresolverr_url").await
}

/// How many CloudFlare solves may run at once (SKADI-T-0488).
///
/// Falls back to the registry default rather than erroring: a malformed value
/// should not stop the daemon publishing providers, and the default is safe.
async fn flaresolverr_max_concurrent(store: &Store) -> usize {
    crate::load_config_view(store)
        .await
        .ok()
        .and_then(|v| v.get_u64("flaresolverr_max_concurrent").ok())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(2)
}

/// The configured cardigann HTTP proxy (empty/unset ⇒ `None`, direct).
async fn cardigann_proxy_url(store: &Store) -> Option<String> {
    config_str(store, "cardigann_proxy_url").await
}

async fn config_str(store: &Store, key: &str) -> Option<String> {
    crate::load_config_view(store)
        .await
        .ok()
        .and_then(|v| v.get_string(key).ok())
        .filter(|s| !s.trim().is_empty())
}

/// Read a provider row's sealed secret, distinguishing "no secret stored" from
/// "the secret is there but cannot be read".
///
/// A credential sealed with a different `SKADI_SECRET_KEY` — after a key rotation,
/// or a key typo — fails to decrypt. That used to propagate out of
/// [`build_providers`] on a `?`, which aborted the whole provider build and, via
/// the supervisor's `?`, stopped every domain from starting (SKADI-T-0519). One
/// unreadable row is a reason to skip that row, not to take the daemon's entire
/// acquisition path down.
///
/// `Err(())` means "skip this row, and it has been logged"; the caller must not
/// treat it as an absent secret, because for a provider whose secret is optional
/// (a public cardigann tracker, a webhook without a signing secret) that would
/// silently build a half-configured provider instead.
/// Hand a secret read from the vault to [`crate::redact`], so its value is
/// masked in check messages and served log lines (SKADI-T-0685).
fn remember(secret: Option<&str>) {
    if let Some(s) = secret {
        crate::redact::remember_secret(s);
    }
}

async fn readable_secret(
    store: &Store,
    kind: &str,
    id: &str,
) -> std::result::Result<Option<String>, ()> {
    match store.get_secret(kind, id).await {
        Ok(v) => {
            remember(v.as_deref());
            Ok(v)
        }
        Err(e) => {
            tracing::error!(
                kind = %kind,
                id = %id,
                error = %e,
                "credential could not be read (wrong SKADI_SECRET_KEY, or sealed with a rotated key); skipping this provider"
            );
            Err(())
        }
    }
}

pub async fn build_providers(store: &Store) -> Result<ProviderSet> {
    let http = HttpClient::new(PROVIDER_HTTP_TIMEOUT)?;
    let mut set = ProviderSet::default();

    // The cardigann definition catalog is loaded lazily on the first cardigann
    // indexer row (most deployments won't have any).
    let defs_dir = cardigann_definitions_dir(store).await;
    let fs_url = flaresolverr_url(store).await;
    // Apply the concurrent-solve cap before any indexer can issue one
    // (SKADI-T-0488). It is process-wide because every indexer shares the one
    // solver container, and this reconcile is the single place that knows the
    // configured value.
    skadi_indexers::flaresolverr::set_max_concurrent(flaresolverr_max_concurrent(store).await);
    let proxy = cardigann_proxy_url(store).await;
    let mut catalog = None;

    // --- indexers ---
    for row in store.list_settings("indexers").await? {
        let Some(id) = parse_id::<IndexerId>("indexers", &row.id) else {
            continue;
        };
        let config: IndexerConfig = match serde_json::from_value(row.body.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(id = %row.id, error = %e, "invalid indexer config; skipping");
                continue;
            }
        };
        // Native cardigann indexers are built from the catalog (the def resolves
        // `definition_id`), not the generic secret+build path.
        if let Some(def_id) = config.cardigann_definition_id().map(str::to_string) {
            if catalog.is_none() {
                match DefinitionStore::new(&defs_dir, http.clone()).load() {
                    Ok(c) => catalog = Some(c),
                    Err(e) => {
                        tracing::warn!(error = %e, "cardigann catalog failed to load; skipping cardigann indexers");
                    }
                }
            }
            let Some(cat) = catalog.as_ref() else {
                continue;
            };
            let Some(def) = cat.get(&def_id) else {
                tracing::warn!(id = %row.id, definition = %def_id, "unknown cardigann definition; skipping");
                continue;
            };
            // Optional sealed secret (login creds for private trackers; absent for public).
            let Ok(secret) = readable_secret(store, "indexers", &row.id).await else {
                continue;
            };
            match config.build_cardigann(id, def, secret, fs_url.as_deref(), proxy.as_deref()) {
                Ok(indexer) => set.indexers.push(Arc::from(indexer)),
                Err(e) => {
                    tracing::warn!(id = %row.id, error = %e, "cardigann indexer failed to build; skipping");
                }
            }
            continue;
        }
        // Built-in indexers (the stub) need no secret; Torznab requires an api
        // key — a missing credential is a skip, not an error.
        let api_key = if config.is_builtin() {
            String::new()
        } else {
            match readable_secret(store, "indexers", &row.id).await {
                Ok(Some(k)) => k,
                Ok(None) => {
                    tracing::warn!(id = %row.id, "indexer has no api_key credential; skipping");
                    continue;
                }
                Err(()) => continue,
            }
        };
        match config.build(id, api_key, http.clone()) {
            Ok(indexer) => set.indexers.push(Arc::from(indexer)),
            Err(e) => tracing::warn!(id = %row.id, error = %e, "indexer failed to build; skipping"),
        }
    }

    // --- downloaders ---
    for row in store.list_settings("downloaders").await? {
        let Some(id) = parse_id::<DownloaderId>("downloaders", &row.id) else {
            continue;
        };
        let config: DownloaderConfig = match serde_json::from_value(row.body.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(id = %row.id, error = %e, "invalid downloader config; skipping");
                continue;
            }
        };
        // The built-in DB-queue downloader is constructed from the store (with
        // its configured download paths), not an endpoint + secret.
        if let Some((incomplete_dir, complete_dir)) = config.skadi_dirs() {
            let repo: Arc<dyn DownloadJobRepo> = Arc::new(store.clone());
            set.downloaders.push(Arc::new(DbDownloader::new(
                id,
                repo,
                incomplete_dir,
                complete_dir,
            )));
            continue;
        }
        let Ok(Some(password)) = readable_secret(store, "downloaders", &row.id).await else {
            tracing::warn!(id = %row.id, "downloader has no usable password credential; skipping");
            continue;
        };
        match config.build(id, password) {
            Ok(downloader) => set.downloaders.push(Arc::from(downloader)),
            Err(e) => {
                tracing::warn!(id = %row.id, error = %e, "downloader failed to build; skipping");
            }
        }
    }

    // --- notifiers ---
    for row in store.list_settings("notifiers").await? {
        let Some(id) = parse_id::<NotifierId>("notifiers", &row.id) else {
            continue;
        };
        let config: NotifierConfig = match serde_json::from_value(row.body.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(id = %row.id, error = %e, "invalid notifier config; skipping");
                continue;
            }
        };
        // The webhook secret is optional — absent is fine.
        let Ok(secret) = readable_secret(store, "notifiers", &row.id).await else {
            continue;
        };
        match config.build(id, secret, http.clone()) {
            Ok(notifier) => set.notifiers.push(Arc::from(notifier)),
            Err(e) => {
                tracing::warn!(id = %row.id, error = %e, "notifier failed to build; skipping");
            }
        }
    }

    tracing::debug!(
        indexers = set.indexers.len(),
        downloaders = set.downloaders.len(),
        notifiers = set.notifiers.len(),
        "provider set built from settings"
    );
    Ok(set)
}

/// A cheap fingerprint of the current provider configuration, for
/// change-detection on the supervisor tick (SKADI-T-0062). Hashes every
/// provider row's `(kind, id, updated_at)` — adds, updates, and deletes all
/// perturb it (the row *set* is part of the hash).
pub async fn provider_fingerprint(store: &Store) -> Result<u64> {
    fingerprint_kinds(store, &["indexers", "downloaders", "notifiers"]).await
}

/// Fingerprint covering providers, the active quality profile (SKADI-T-0067),
/// **and** the `config` table (SKADI-I-0014). The supervisor uses this so a
/// provider, profile, **or config** change triggers a reload — that is the
/// "hot" path: a config edit bumps this hash and the next tick re-reads it.
/// The `config` keys a rebuilt provider set actually depends on (SKADI-T-0459).
///
/// The fingerprint used to hash the **whole** config table, so writing any key —
/// a naming template, a download path, `backup.dir` — changed it and made the
/// supervisor rebuild every provider and re-apply it to every reloader. That is
/// wasted work on a tick that runs every few seconds, and it re-publishes a
/// provider set that has not changed.
///
/// Keep this in step with what `build_providers` reads: the definitions
/// directory, and the two endpoints the cardigann fetcher is constructed with.
/// Anything not listed here cannot change a provider, so it must not force a
/// rebuild. `custom_formats` and the profile rows are covered by the settings
/// fingerprint below, not by config.
const PROVIDER_CONFIG_KEYS: &[&str] = &[
    "cardigann_definitions_dir",
    "flaresolverr_url",
    "cardigann_proxy_url",
];

pub async fn service_fingerprint(store: &Store) -> Result<u64> {
    use std::hash::{Hash, Hasher};
    let kinds =
        fingerprint_kinds(store, &["indexers", "downloaders", "notifiers", "profiles"]).await?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kinds.hash(&mut h);
    for key in PROVIDER_CONFIG_KEYS {
        key.hash(&mut h);
        // Absent and empty hash alike, which is right: both mean "not
        // configured", and `config_str` already treats them the same.
        config_str(store, key)
            .await
            .unwrap_or_default()
            .hash(&mut h);
    }
    Ok(h.finish())
}

/// Hash every row's `(kind, id, updated_at)` across the given settings kinds.
async fn fingerprint_kinds(store: &Store, kinds: &[&str]) -> Result<u64> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for kind in kinds {
        for row in store.list_settings(kind).await? {
            kind.hash(&mut h);
            row.id.hash(&mut h);
            row.updated_at.to_rfc3339().hash(&mut h);
        }
    }
    Ok(h.finish())
}

/// Applies a freshly-built [`ProviderSet`] to a running domain (SKADI-T-0062).
///
/// Domains implement this (movies: swap the providers into its shared deps and
/// re-publish `HunterServices`); the supervisor calls it when the provider
/// settings fingerprint changes. Same dependency-inversion pattern as
/// [`HttpModule`](crate::http_module::HttpModule) /
/// [`LibraryProvider`](crate::library::LibraryProvider).
#[async_trait::async_trait]
pub trait ProviderReloader: Send + Sync {
    /// Install the new provider set; takes effect for the next workflow stage.
    async fn apply(&self, set: ProviderSet) -> Result<()>;
}

/// One built provider, type-erased for the test-connection handler.
pub enum OneProvider {
    Indexer(Arc<dyn Indexer>),
    Downloader(Arc<dyn Downloader>),
    Notifier(Arc<dyn Notifier>),
}

impl OneProvider {
    /// Run the provider's connectivity/credential check (SKADI-T-0061).
    pub async fn test(&self) -> Result<()> {
        match self {
            OneProvider::Indexer(p) => p.test().await,
            OneProvider::Downloader(p) => p.test().await,
            OneProvider::Notifier(p) => p.test().await,
        }
    }
}

/// Build a single provider from its stored settings row + credential — strict
/// (errors instead of skipping), for the `POST /settings/{kind}/{id}/test`
/// handler where the user wants to know exactly what's wrong.
pub async fn build_one(store: &Store, kind: &str, id: &str) -> Result<OneProvider> {
    use skadi_core::AppError;

    let row = store
        .get_setting(kind, id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("{kind}/{id} not found")))?;
    let http = HttpClient::new(PROVIDER_HTTP_TIMEOUT)?;
    let secret = store.get_secret(kind, id).await?;
    remember(secret.as_deref());

    match kind {
        "indexers" => {
            let config: IndexerConfig = serde_json::from_value(row.body)
                .map_err(|e| AppError::Validation(format!("invalid indexer config: {e}")))?;
            let typed = parse_id::<IndexerId>(kind, id)
                .ok_or_else(|| AppError::Validation(format!("{kind}/{id}: id is not a UUID")))?;
            // Native cardigann: resolve the definition from the catalog + build.
            if let Some(def_id) = config.cardigann_definition_id().map(str::to_string) {
                let dir = cardigann_definitions_dir(store).await;
                let catalog = DefinitionStore::new(&dir, http.clone()).load()?;
                let def = catalog.get(&def_id).ok_or_else(|| {
                    AppError::Validation(format!("unknown cardigann definition {def_id:?}"))
                })?;
                let fs_url = flaresolverr_url(store).await;
                let proxy = cardigann_proxy_url(store).await;
                return Ok(OneProvider::Indexer(Arc::from(config.build_cardigann(
                    typed,
                    def,
                    secret,
                    fs_url.as_deref(),
                    proxy.as_deref(),
                )?)));
            }
            // The built-in stub indexer needs no secret.
            let api_key = if config.is_builtin() {
                String::new()
            } else {
                secret.ok_or_else(|| {
                    AppError::Validation("indexer has no api_key credential set".into())
                })?
            };
            Ok(OneProvider::Indexer(Arc::from(
                config.build(typed, api_key, http)?,
            )))
        }
        "downloaders" => {
            let config: DownloaderConfig = serde_json::from_value(row.body)
                .map_err(|e| AppError::Validation(format!("invalid downloader config: {e}")))?;
            let typed = parse_id::<DownloaderId>(kind, id)
                .ok_or_else(|| AppError::Validation(format!("{kind}/{id}: id is not a UUID")))?;
            // The built-in DB-queue downloader needs no secret — build it from
            // the store directly, with its configured download paths.
            if let Some((incomplete_dir, complete_dir)) = config.skadi_dirs() {
                let repo: Arc<dyn DownloadJobRepo> = Arc::new(store.clone());
                return Ok(OneProvider::Downloader(Arc::new(DbDownloader::new(
                    typed,
                    repo,
                    incomplete_dir,
                    complete_dir,
                ))));
            }
            let password = secret.ok_or_else(|| {
                AppError::Validation("downloader has no password credential set".into())
            })?;
            Ok(OneProvider::Downloader(Arc::from(
                config.build(typed, password)?,
            )))
        }
        "notifiers" => {
            let config: NotifierConfig = serde_json::from_value(row.body)
                .map_err(|e| AppError::Validation(format!("invalid notifier config: {e}")))?;
            let typed = parse_id::<NotifierId>(kind, id)
                .ok_or_else(|| AppError::Validation(format!("{kind}/{id}: id is not a UUID")))?;
            Ok(OneProvider::Notifier(Arc::from(
                config.build(typed, secret, http)?,
            )))
        }
        other => Err(AppError::NotFound(format!(
            "settings kind {other:?} is not a testable provider"
        ))),
    }
}
