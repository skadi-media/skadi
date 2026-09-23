//! The process-global service registry.
//!
//! Cloacina injects nothing into a task beyond its `Context<serde_json::Value>`
//! — there is no resources/extensions channel, and the store, HTTP client,
//! indexers, downloaders and notifiers are not serializable. But tasks are
//! ordinary fns compiled into our binary and run on tokio threads **in our
//! process**, so a process-global is reachable from a task body. That is the
//! one sanctioned exception to "everything flows through the context":
//! capabilities live here, serde data lives in [`crate::state::AcquireState`].
//!
//! `HunterWorker` (T-0034) calls [`set_services`] exactly once at startup.
//! Tasks call [`services`]. Tests use [`reset_services`] between cases (the test
//! harness runs single-threaded, so a resettable global is safe).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use async_trait::async_trait;
use skadi_core::{MediaKind, Result};
use skadi_downloaders::Downloader;
use skadi_importer::{AcquirableRef, Importer};
use skadi_indexers::Indexer;
use skadi_notify::Notifier;
use skadi_quality::audiobook::AudiobookQualityDefinition;
use skadi_quality::{CustomFormat, FormatRule, QualityDefinition, QualityProfile};
use skadi_store::{SettingsRepo, Store};

use crate::status::StatusSink;

/// Build a per-run [`Importer`] for a specific [`AcquirableRef`].
///
/// Domains whose matcher needs the per-run movie/edition snapshot (movies do —
/// see SKADI-I-0007's `MovieMatcher`) install one of these on
/// [`HunterServices::importer_factory`]; the workflow's `import_task` calls
/// `for_acquirable(...)` before invoking [`crate::pipeline::import`], so the
/// returned importer has everything baked in by the time `match_file` runs.
/// When unset, the workflow falls back to the static
/// [`HunterServices::importer`].
#[async_trait]
pub trait ImporterFactory: Send + Sync {
    async fn for_acquirable(&self, acquirable: &AcquirableRef) -> Result<Arc<dyn Importer>>;
}

/// The non-serializable capabilities a workflow task may need. Holds long-lived
/// handles; lookups against stored config happen here (e.g. resolving the
/// active profile) so tasks stay thin and runner-free.
pub struct HunterServices {
    /// Which domain these services belong to. The registry is keyed by this, and
    /// a workflow task resolves its services via `services_for(state.request.kind)`
    /// — so movies and audiobooks (and future domains) coexist (SKADI-T-0136).
    pub kind: MediaKind,
    /// The skadi database handle (config, credentials, domain repos).
    pub store: Store,
    /// Where acquisition status is persisted at task boundaries.
    pub status: Arc<dyn StatusSink>,
    /// Enabled indexers (the `search` task filters by `supports(kind)`).
    pub indexers: Vec<Arc<dyn Indexer>>,
    /// Enabled downloaders (the `snatch`/`monitor` tasks pick by protocol).
    pub downloaders: Vec<Arc<dyn Downloader>>,
    /// Importer the `import` task delegates to **when no
    /// [`importer_factory`](Self::importer_factory) is set**. Movies and other
    /// domains whose matcher needs per-run context install a factory instead.
    pub importer: Arc<dyn Importer>,
    /// Optional per-run importer resolver. When `Some`, the workflow's
    /// `import_task` calls `factory.for_acquirable(state.acquirable)` and uses
    /// the returned importer for that run. When `None`, falls back to
    /// [`Self::importer`].
    pub importer_factory: Option<Arc<dyn ImporterFactory>>,
    /// Notifiers the `notify` task fans events out to.
    pub notifiers: Vec<Arc<dyn Notifier>>,
    /// Scoring configuration the `decide` task consults.
    pub scoring: ScoringConfig,
}

/// Quality-side configuration the `decide` task needs: the quality definitions
/// to classify a parsed release, the profile to gate/rank, and the custom-format
/// registry. v0 holds these directly; T-0034/the daemon initiative replaces the
/// single `profile` with store-backed lookup by `ProfileId`.
#[derive(Clone)]
pub struct ScoringConfig {
    pub definitions: Vec<QualityDefinition>,
    pub profile: QualityProfile,
    pub formats: Vec<CustomFormat>,
    /// Minimum seeders a **torrent** release must report to be eligible in
    /// `decide` (SKADI-T-0113). A torrent below this is skipped (dead/too-slow);
    /// usenet/NZB releases carry no seeders and are never filtered. `0` disables
    /// the filter.
    pub min_seeders: u32,
    /// When `Some`, this domain scores **audiobook** releases (SKADI-I-0017):
    /// `evaluate` re-parses each release title with `parse_audiobook`, classifies
    /// it against these definitions, and applies the abridged-reject floor. A
    /// movie domain leaves this `None` and uses `definitions` (resolution/source).
    pub audiobook: Option<AudiobookScoring>,
}

/// Audiobook-axis scoring config (SKADI-I-0017): the ranked audiobook quality
/// definitions a release is classified against, plus whether abridged releases
/// are acceptable.
#[derive(Clone)]
pub struct AudiobookScoring {
    pub definitions: Vec<AudiobookQualityDefinition>,
    /// Reject abridged releases unless `true`.
    pub allow_abridged: bool,
}

/// The stored shape of a `custom_formats` settings row (SKADI-T-0183). The
/// settings row id is the [`CustomFormatId`](skadi_core::CustomFormatId) that a
/// profile's `CustomFormatScore` entries reference. `domain` keys the format to
/// one media domain — skadi is one all-in-one app, so each domain loads only its
/// own formats (a movie format never scores an audiobook release).
#[derive(serde::Deserialize)]
struct CustomFormatSpec {
    domain: MediaKind,
    name: String,
    rules: Vec<FormatRule>,
}

/// Load the custom-format **definitions** for `domain` from the `custom_formats`
/// settings into a scoring registry (SKADI-T-0183). Only this domain's rows are
/// loaded, so a movie format never scores an audiobook release and vice-versa.
/// The row id becomes the [`CustomFormatId`](skadi_core::CustomFormatId) profiles
/// reference. A malformed row is skipped (logged), never failing the whole load —
/// one bad row can't break scoring. Shared by every domain module so the load +
/// domain-filter rule lives in exactly one place.
/// The stored shape of a `profiles` settings row, as written by the API's
/// normaliser (which fills sparse bodies, so every stored row is complete).
#[derive(serde::Deserialize)]
struct StoredProfile {
    #[serde(default)]
    name: Option<String>,
    /// Allowed quality definition ids, ordered low→high (rank = index).
    allowed: Vec<uuid::Uuid>,
    /// At/above this quality, no further upgrades are pursued. Must be in `allowed`.
    cutoff: uuid::Uuid,
    #[serde(default)]
    upgrade_allowed: bool,
    #[serde(default)]
    min_format_score: i32,
    #[serde(default)]
    formats: Vec<skadi_quality::CustomFormatScore>,
}

/// Resolve the quality profile an item was actually assigned (SKADI-T-0531).
///
/// Each acquirable carries its own `ProfileId`, but `decide` used to score every
/// item against one domain-wide profile — whichever `profiles` row came back
/// first. With the six seeded profiles (Any / SD / HD-720p / HD-1080p /
/// HD-720p-1080p / Ultra-HD) that made the quality gate for the whole library
/// depend on row order: land on SD and every 1080p release in the library is
/// rejected as "quality", with nothing to point at.
///
/// Returns `None` when the row is missing or fails validation, so the caller can
/// fall back to the domain default rather than silently accepting a broken
/// profile. Validation matches the API's write-time rules: every allowed quality
/// must be known, and the cutoff must be one of them.
pub async fn resolve_profile_by_id(
    store: &Store,
    id: skadi_core::ProfileId,
    defs: &[QualityDefinition],
) -> Option<QualityProfile> {
    let row = store
        .get_setting("profiles", &id.to_string())
        .await
        .ok()
        .flatten()?;
    let spec: StoredProfile = serde_json::from_value(row.body).ok()?;
    let known: std::collections::HashSet<uuid::Uuid> =
        defs.iter().map(|d| d.id.into_uuid()).collect();
    if spec.allowed.is_empty()
        || !spec.allowed.iter().all(|q| known.contains(q))
        || !spec.allowed.contains(&spec.cutoff)
    {
        tracing::warn!(
            profile = %id,
            "profile references unknown qualities or a cutoff outside allowed; using the domain default"
        );
        return None;
    }
    Some(QualityProfile {
        id,
        name: spec.name.unwrap_or_else(|| "profile".into()),
        allowed: spec
            .allowed
            .into_iter()
            .map(skadi_core::QualityId::from)
            .collect(),
        cutoff: skadi_core::QualityId::from(spec.cutoff),
        upgrade_allowed: spec.upgrade_allowed,
        formats: spec.formats,
        min_format_score: spec.min_format_score,
    })
}

pub async fn resolve_custom_formats(store: &Store, domain: MediaKind) -> Vec<CustomFormat> {
    let rows = match store.list_settings("custom_formats").await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "reading custom_formats settings; none loaded");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for row in rows {
        let spec: CustomFormatSpec = match serde_json::from_value(row.body.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(id = %row.id, error = %e, "invalid custom_format body; skipping");
                continue;
            }
        };
        if spec.domain != domain {
            continue;
        }
        let Ok(uuid) = uuid::Uuid::parse_str(&row.id) else {
            tracing::warn!(id = %row.id, "custom_format id is not a uuid; skipping");
            continue;
        };
        out.push(CustomFormat {
            id: skadi_core::CustomFormatId::from(uuid),
            name: spec.name,
            rules: spec.rules,
        });
    }
    if !out.is_empty() {
        tracing::info!(domain = ?domain, count = out.len(), "custom formats loaded from settings");
    }
    out
}

// A per-`MediaKind` registry (SKADI-T-0136), so multiple domains coexist in one
// process. Resettable for tests; production registers one entry per domain at
// each domain's `HunterWorker` start.
static HUNTER: LazyLock<RwLock<HashMap<MediaKind, Arc<HunterServices>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Register `services` for its [`HunterServices::kind`]. Returns the `Arc` for
/// convenience. Replaces any prior entry for that kind (a provider reload
/// re-publishes the same domain's services).
pub fn set_services(services: Arc<HunterServices>) -> Arc<HunterServices> {
    HUNTER
        .write()
        .unwrap()
        .insert(services.kind, services.clone());
    services
}

/// The registered services for `kind`.
///
/// # Panics
/// If no services were registered for `kind` — a task ran before its domain's
/// worker initialized the registry, which is a programming error.
#[must_use]
pub fn services_for(kind: MediaKind) -> Arc<HunterServices> {
    if let Some(s) = try_services_for(kind) {
        return s;
    }
    // Startup-race tolerance (SKADI-T-0355): after a restart, Cloacina's runner
    // resumes any persisted acquire task the instant it starts — which can be a
    // beat BEFORE the domain's `HunterWorker` calls `set_services()` during boot.
    // The bare panic was crash-LOOPING the daemon: once the VPN/indexers came
    // back up, resumed audiobook acquires actually reached a `services_for` call
    // inside that window and aborted the process, which restarted and repeated.
    // Cloacina runs task bodies on its OWN executor threads (separate from the
    // bootstrap task that registers services), so a short bounded wait here parks
    // only a worker thread while registration completes — it does not block the
    // boot path. A genuinely never-registered domain still panics after the grace,
    // preserving the original "programming error" contract.
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if let Some(s) = try_services_for(kind) {
            return s;
        }
    }
    panic!("HunterServices not initialized for {kind:?}; call set_services() (waited 10s)")
}

/// The registered services for `kind`, or `None` if not yet set.
#[must_use]
pub fn try_services_for(kind: MediaKind) -> Option<Arc<HunterServices>> {
    HUNTER.read().unwrap().get(&kind).cloned()
}

/// Clear the registry (for tests).
pub fn reset_services() {
    HUNTER.write().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::InMemoryStatusSink;
    use skadi_importer::{AcquirableMatch, AcquirableMatcher, CompletedDownload, DefaultImporter};
    use skadi_quality::{ParsedRelease, QualityProfile, default_definitions};
    use std::path::Path;

    /// A do-nothing matcher used by the test services scaffold.
    struct NoMatcher;
    impl AcquirableMatcher for NoMatcher {
        fn match_file(
            &self,
            _parsed: &ParsedRelease,
            _source: &Path,
            _completed: &CompletedDownload,
        ) -> Vec<AcquirableMatch> {
            vec![]
        }
    }

    fn empty_services() -> Arc<HunterServices> {
        empty_services_kind(MediaKind::Movie)
    }

    fn empty_services_kind(kind: MediaKind) -> Arc<HunterServices> {
        let store = Store::connect("sqlite://:memory:").unwrap();
        let defs = default_definitions();
        let any_q = defs[0].id;
        let profile = QualityProfile {
            id: skadi_core::ProfileId::new(),
            name: "t".into(),
            allowed: vec![any_q],
            cutoff: any_q,
            upgrade_allowed: false,
            formats: vec![],
            min_format_score: 0,
        };
        Arc::new(HunterServices {
            kind,
            store,
            status: Arc::new(InMemoryStatusSink::new()),
            indexers: vec![],
            downloaders: vec![],
            importer: Arc::new(DefaultImporter::new(NoMatcher)),
            importer_factory: None,
            notifiers: vec![],
            scoring: ScoringConfig {
                definitions: defs,
                profile,
                formats: vec![],
                min_seeders: 0,
                audiobook: None,
            },
        })
    }

    // `#[serial]` on every registry-touching test (SKADI-T-0383): they share the
    // process-global `HUNTER` map, so run in parallel one test's
    // `reset_services()` lands between another's `set_services` and its
    // assertion — and the loser then blocks for the full `services_for` grace
    // (10s) before panicking. Serializing them is what makes this suite
    // deterministic.
    #[tokio::test]
    #[serial_test::serial(hunter_registry)]
    async fn set_then_get_returns_the_registry_by_kind() {
        reset_services();
        assert!(try_services_for(MediaKind::Movie).is_none());
        let svc = empty_services();
        set_services(svc.clone());
        assert!(Arc::ptr_eq(&services_for(MediaKind::Movie), &svc));
        // A different kind is independent (not set).
        assert!(try_services_for(MediaKind::Audiobook).is_none());
        reset_services();
        assert!(try_services_for(MediaKind::Movie).is_none());
    }

    #[tokio::test]
    #[serial_test::serial(hunter_registry)]
    async fn two_domains_coexist_in_the_registry() {
        reset_services();
        let movie = empty_services(); // kind = Movie
        set_services(movie.clone());
        // A second domain's services under a different kind.
        let ab = empty_services_kind(MediaKind::Audiobook);
        set_services(ab.clone());
        assert!(Arc::ptr_eq(&services_for(MediaKind::Movie), &movie));
        assert!(Arc::ptr_eq(&services_for(MediaKind::Audiobook), &ab));
        reset_services();
    }

    // --- domain-keyed custom-format loading (SKADI-T-0183) ---

    async fn fresh_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("s.db").display());
        let s = Store::connect(&url).unwrap();
        s.run_migrations().await.unwrap();
        (dir, s)
    }

    #[tokio::test]
    async fn an_item_is_scored_against_its_own_profile_row() {
        // SKADI-T-0531: `decide` used to score every item against one domain-wide
        // profile — whichever `profiles` row came back first. With the six seeded
        // profiles that made the quality gate depend on row order, so an SD
        // profile could silently reject every 1080p release in the library.
        let (_d, store) = fresh_store().await;
        let defs = default_definitions();
        let sd = defs.iter().find(|d| d.name == "SDTV").unwrap().id;
        let hd = defs.iter().find(|d| d.name == "Bluray-1080p").unwrap().id;

        let id = skadi_core::ProfileId::new();
        store
            .put_setting(
                "profiles",
                &id.to_string(),
                &serde_json::json!({
                    "name": "HD only",
                    "allowed": [hd.to_string()],
                    "cutoff": hd.to_string(),
                    "upgrade_allowed": true,
                }),
            )
            .await
            .unwrap();

        let got = resolve_profile_by_id(&store, id, &defs)
            .await
            .expect("resolves");
        assert_eq!(got.id, id, "the resolved profile keeps the row's id");
        assert_eq!(got.name, "HD only");
        assert_eq!(got.allowed, vec![hd]);
        assert!(!got.allowed.contains(&sd), "an SD row must not leak in");
    }

    #[tokio::test]
    async fn a_missing_or_invalid_profile_falls_back_rather_than_resolving() {
        let (_d, store) = fresh_store().await;
        let defs = default_definitions();

        // Missing row.
        let absent = skadi_core::ProfileId::new();
        assert!(resolve_profile_by_id(&store, absent, &defs).await.is_none());

        // A cutoff outside `allowed` is the shape the API rejects at write time;
        // if one is in the database anyway, resolving must decline it so the
        // caller falls back instead of scoring against a broken profile.
        let hd = defs.iter().find(|d| d.name == "Bluray-1080p").unwrap().id;
        let sd = defs.iter().find(|d| d.name == "SDTV").unwrap().id;
        let bad = skadi_core::ProfileId::new();
        store
            .put_setting(
                "profiles",
                &bad.to_string(),
                &serde_json::json!({
                    "name": "broken",
                    "allowed": [hd.to_string()],
                    "cutoff": sd.to_string(),
                }),
            )
            .await
            .unwrap();
        assert!(resolve_profile_by_id(&store, bad, &defs).await.is_none());

        // And an empty allowed list, which can never accept any release.
        let empty = skadi_core::ProfileId::new();
        store
            .put_setting(
                "profiles",
                &empty.to_string(),
                &serde_json::json!({ "name": "empty", "allowed": [], "cutoff": hd.to_string() }),
            )
            .await
            .unwrap();
        assert!(resolve_profile_by_id(&store, empty, &defs).await.is_none());
    }

    #[tokio::test]
    async fn resolve_custom_formats_filters_by_domain_and_skips_bad_rows() {
        let (_d, store) = fresh_store().await;
        let movie_id = uuid::Uuid::new_v4();
        store
            .put_setting(
                "custom_formats",
                &movie_id.to_string(),
                &serde_json::json!({ "domain": "Movie", "name": "x265", "rules": [{ "Codec": "x265" }] }),
            )
            .await
            .unwrap();
        store
            .put_setting(
                "custom_formats",
                &uuid::Uuid::new_v4().to_string(),
                &serde_json::json!({ "domain": "Audiobook", "name": "M4B", "rules": [{ "TitleRegex": "m4b" }] }),
            )
            .await
            .unwrap();
        // A malformed row (no domain) is skipped, not fatal.
        store
            .put_setting(
                "custom_formats",
                &uuid::Uuid::new_v4().to_string(),
                &serde_json::json!({ "name": "broken" }),
            )
            .await
            .unwrap();

        // Movies load only their own domain's format; the audiobook one is excluded.
        let movie = resolve_custom_formats(&store, MediaKind::Movie).await;
        assert_eq!(movie.len(), 1, "only the Movie-domain format loads");
        assert_eq!(movie[0].name, "x265");
        assert_eq!(
            movie[0].id.into_uuid(),
            movie_id,
            "row id becomes the format id"
        );
        assert_eq!(movie[0].rules, vec![FormatRule::Codec("x265".into())]);

        // And audiobooks load only theirs — domain isolation.
        let ab = resolve_custom_formats(&store, MediaKind::Audiobook).await;
        assert_eq!(ab.len(), 1);
        assert_eq!(ab[0].name, "M4B");
    }
}
