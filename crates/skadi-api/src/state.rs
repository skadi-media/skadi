//! The shared application state handlers see via [`axum::extract::State`].
//!
//! Cheap to clone (everything inside is `Arc` or already cheaply cloneable),
//! so it's wrapped in an `Arc<AppState>` once in [`serve`](crate::serve) and
//! handed to every handler. Later I-0008 tasks fill the currently-empty slots
//! (domain registry, cloacina runners, hunter services).

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use skadi_core::MediaKind;
use skadi_store::Store;

use crate::config::Config;

/// A compiled-in domain's identity, as the daemon registers it. The full
/// [`DomainModule`](skadi_core::DomainModule) lives in the supervisor; the API
/// only needs name + kind to render `/domains`.
#[derive(Clone, Debug)]
pub struct DomainDescriptor {
    pub name: String,
    pub kind: MediaKind,
}

/// State shared across all request handlers.
///
/// The `store` is optional in this skeleton: SKADI-T-0050 stands the crate up
/// before SKADI-T-0051 wires bootstrap, so the scaffold (and its handler tests)
/// can construct an `AppState` without a live database. Once bootstrap lands,
/// `serve` is always called with `Some(store)`.
#[derive(Clone)]
pub struct AppState {
    /// The runtime configuration.
    pub config: Arc<Config>,
    /// The backing store, once bootstrap has connected it.
    pub store: Option<Store>,
    /// The compiled-in domains, for the `/domains` endpoint.
    pub domains: Vec<DomainDescriptor>,
    /// Per-domain library providers, for the `/library` endpoint
    /// (SKADI-T-0055). Empty unless the daemon registered any.
    pub library: Vec<Arc<dyn crate::library::LibraryProvider>>,
    /// Import-list providers, for the sync engine (SKADI-T-0511). Empty unless
    /// the daemon registered any, in which case a list of an unregistered kind
    /// fails with that reason rather than silently syncing nothing.
    pub import_list_providers: Vec<Arc<dyn skadi_metadata::import_list::ImportListProvider>>,
    /// Daemon-wide shutdown signal; handlers and background tasks observe it.
    pub cancel: CancellationToken,
    /// Set once the supervisor has completed a provider reconcile
    /// (SKADI-T-0475). The readiness probe reads it, so a deploy gate can tell
    /// "the listener is up" from "the daemon can actually do its job". Shared
    /// with the supervisor; never cleared once set — a later reconcile failure
    /// is reported through health checks, not by making the daemon un-ready.
    pub providers_ready: Arc<std::sync::atomic::AtomicBool>,
    /// Per-domain count of workers that ended without being asked to
    /// (SKADI-T-0523), shared with the supervisor the same way `providers_ready`
    /// is. A climbing count is how `/system/status` can tell a crash-looping
    /// domain from a healthy one — the supervisor restarts it each tick, so a
    /// running/not-running flag alone shows it as fine.
    pub worker_failures: Arc<tokio::sync::Mutex<std::collections::HashMap<String, u64>>>,
    /// The API token as it stands **right now** (SKADI-T-0466), refreshed from
    /// the `config` table by the supervisor tick.
    ///
    /// `Config::bearer_token` is resolved once at startup, so rotating
    /// `api_token` used to require a daemon restart — during which the old token
    /// kept working and the new one did not, which is the wrong way round for a
    /// credential you are rotating *because* it leaked.
    ///
    /// `None` means open mode. Seeded from `config.bearer_token` so behaviour is
    /// unchanged until the first refresh.
    pub live_token: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Household members (SKADI-T-0611), synced from the `members` settings
    /// records; the admin mirrors `live_token`.
    pub members: Arc<tokio::sync::RwLock<crate::household::MemberDirectory>>,
    /// Widening delay on repeated failed logins (SKADI-T-0620).
    pub login_throttle: Arc<crate::household::LoginThrottle>,
    /// Recent log lines, for `GET /log` (SKADI-T-0467). Empty unless the daemon
    /// installed [`logbuf::LogBuffer::layer`] on its subscriber — tests and the
    /// CLI's one-shot commands do not.
    pub logs: crate::logbuf::LogBuffer,
    /// The health checks and their last results (SKADI-T-0680).
    /// `GET /health/checks` reads it; the supervisor tick refreshes it
    /// ([`AppState::refresh_health`]).
    pub health: Arc<crate::health_checks::HealthCache>,
    /// How the `disk-space` health check measures the library root
    /// (SKADI-T-0681): `statvfs`, or a fixed fill level in a test.
    pub disk_probe: crate::health_checks::DiskProbe,
}

impl AppState {
    /// Build an `AppState` from a [`Config`] and an optional [`Store`], with a
    /// fresh [`CancellationToken`] and no registered domains.
    pub fn new(config: Config, store: Option<Store>) -> Arc<Self> {
        Self::new_with_domains(config, store, Vec::new())
    }

    /// Build an `AppState` advertising the given compiled-in domains (no library
    /// providers).
    pub fn new_with_domains(
        config: Config,
        store: Option<Store>,
        domains: Vec<DomainDescriptor>,
    ) -> Arc<Self> {
        Self::new_full(config, store, domains, Vec::new())
    }

    /// Build a fully-specified `AppState` (used by the daemon, SKADI-T-0056).
    /// Re-read `api_token` from the config table and publish it (SKADI-T-0466).
    ///
    /// Called by the supervisor each tick, so a rotated token applies within one
    /// reconcile interval rather than at the next daemon restart — during which
    /// the old token kept working and the new one did not, which is the wrong way
    /// round for a credential being rotated because it leaked.
    ///
    /// Deliberately quiet about the value: a change is logged as *that* it
    /// changed, never what to. A read failure leaves the current token in place —
    /// a transient database blip must not silently drop the daemon into open mode.
    pub async fn refresh_api_token(&self) {
        use skadi_store::ConfigRepo;
        let Some(store) = &self.store else { return };
        let Ok(entry) = store.get_config("api_token").await else {
            return;
        };
        let next = entry.map(|e| e.value).filter(|v| !v.trim().is_empty());
        let mut cur = self.live_token.write().await;
        if *cur != next {
            tracing::info!(
                open_mode = next.is_none(),
                "api_token changed — applying without a restart"
            );
            *cur = next;
        }
    }

    /// Run the health checks whose results are older than their TTL, and store
    /// the results (SKADI-T-0680). The supervisor tick spawns it; it returns when
    /// those checks have finished. Without a store there is nothing to check.
    pub async fn refresh_health(&self) {
        if let Ok(ctx) = crate::health_checks::CheckContext::from_state(self) {
            self.health.refresh_due(&ctx).await;
        }
    }

    pub fn new_full(
        config: Config,
        store: Option<Store>,
        domains: Vec<DomainDescriptor>,
        library: Vec<Arc<dyn crate::library::LibraryProvider>>,
    ) -> Arc<Self> {
        // Seed the live token from the resolved config so behaviour is unchanged
        // until the supervisor's first refresh (SKADI-T-0466).
        let live_token = Arc::new(tokio::sync::RwLock::new(config.bearer_token.clone()));
        Arc::new(Self {
            config: Arc::new(config),
            store,
            domains,
            library,
            import_list_providers: Vec::new(),
            cancel: CancellationToken::new(),
            providers_ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            worker_failures: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            live_token,
            login_throttle: Arc::new(crate::household::LoginThrottle::default()),
            members: Arc::new(tokio::sync::RwLock::new(
                crate::household::MemberDirectory::default(),
            )),
            logs: crate::logbuf::LogBuffer::new(),
            health: Arc::new(crate::health_checks::HealthCache::new(
                crate::health_checks::HealthRegistry::builtin(),
            )),
            disk_probe: crate::health_checks::statvfs_probe(),
        })
    }
}
