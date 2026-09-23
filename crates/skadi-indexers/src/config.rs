//! Typed indexer configuration (SKADI-T-0058).
//!
//! [`IndexerConfig`] is the exact non-secret shape stored in the daemon's
//! settings `body` for `kind = "indexers"` rows. The settings API stores it,
//! the provider factory deserializes it and calls [`IndexerConfig::build`] with
//! the secret (sealed separately in the credential store) to produce a live
//! [`Indexer`].
//!
//! Tagged by `kind` so future indexer protocols slot in as new variants:
//! `{ "kind": "torznab", "name": "...", "base_url": "...", "categories": [2000] }`

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use skadi_cardigann::Definition;
use skadi_core::{AppError, IndexerId, Result};
use skadi_http::HttpClient;

use crate::cardigann::CardigannIndexer;
use crate::health::indexer_health_tracked;
use crate::knaben::{KNABEN_API_URL, Knaben};
use crate::prowlarr::Prowlarr;
use crate::ratelimit::{DEFAULT_RATE_PER_MINUTE, indexer_rate_limited};
use crate::stub::StubIndexer;
use crate::torznab::Torznab;
use crate::{Category, Indexer};

/// Non-secret configuration for one indexer, as stored in settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IndexerConfig {
    /// A Torznab/Newznab endpoint. The API key is **not** part of this config —
    /// it lives in the credential store and is passed to [`Self::build`].
    Torznab(TorznabConfig),
    /// A **Prowlarr aggregate** endpoint (SKADI-T-0179): one config that searches
    /// *all* of Prowlarr's indexers via its JSON `/api/v1/search` API. The API key
    /// lives in the credential store, like Torznab.
    Prowlarr(ProwlarrConfig),
    /// A **native Cardigann** tracker (SKADI-I-0036): driven by a synced
    /// definition (`definition_id`) from the catalog + the user's non-secret
    /// `settings`. Built via [`Self::build_cardigann`] (it needs the resolved
    /// definition, which the provider factory looks up from the catalog).
    Cardigann(CardigannConfig),
    /// **Knaben** (SKADI-T-0618): a public meta-search over the open trackers,
    /// via its own JSON API. No account and therefore no credential — see
    /// [`Self::is_builtin`].
    Knaben(KnabenConfig),
    /// The built-in **stub** indexer (SKADI-T-0104): canned open-movie releases
    /// for `testing` mode, no external service or secret. Not surfaced as a
    /// user-creatable kind in the production UI — seeded only by the
    /// testing-mode preset (parallels the built-in `skadi` downloader).
    Stub(StubConfig),
}

/// Sonarr's per-indexer flags (SKADI-T-0505), shared by every configurable kind.
///
/// Flattened into each config so a stored row carries them at the top level, the
/// way Sonarr's does — rather than nesting them and forcing every consumer to
/// know about a wrapper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexerFlags {
    /// Include this indexer in the RSS sweep. Off is how an operator keeps a slow
    /// or rate-limited tracker for explicit searches without it being polled every
    /// few minutes.
    #[serde(default = "yes")]
    pub enable_rss: bool,
    /// Include this indexer in automatic (sweep-driven) searches. Off leaves it
    /// available for an interactive search the operator drives by hand.
    #[serde(default = "yes")]
    pub enable_automatic_search: bool,
    /// Tie-break order across indexers, lower first (Sonarr's 1-50, default 25).
    ///
    /// **Stored but not yet consulted** — see SKADI-T-0539. Recorded now so an
    /// operator's setting survives, rather than being silently discarded and
    /// having to be entered again once ranking uses it.
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// Per-indexer seeder floor overriding the profile's; `0` ⇒ use the
    /// profile's (Sonarr's convention, and why this is a `u32` rather than an
    /// `Option` — the field is always present so an operator can see what it is).
    ///
    /// **Stored but not yet consulted** — see SKADI-T-0539.
    #[serde(default)]
    pub minimum_seeders: u32,
    /// Tag ids scoping this indexer to matching items (SKADI-T-0556).
    ///
    /// **Empty means "applies to every item"**, not "applies to none". That is
    /// the Sonarr semantic and the only safe default: every existing install has
    /// untagged indexers, so the inverted reading would silently take every one
    /// of them out of the fan-out the moment this shipped, and the symptom would
    /// be "searches stopped finding anything" with no error anywhere.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl IndexerFlags {
    /// Whether this indexer applies to an item carrying `item_tags`
    /// (SKADI-T-0556).
    ///
    /// Untagged indexer ⇒ always. Tagged ⇒ only when the item shares at least
    /// one tag, matching Sonarr: an indexer tagged `anime` is queried for
    /// anime-tagged items and no others.
    #[must_use]
    pub fn applies_to(&self, item_tags: &[String]) -> bool {
        self.tags.is_empty() || self.tags.iter().any(|t| item_tags.contains(t))
    }
}

fn yes() -> bool {
    true
}

fn default_priority() -> u32 {
    25
}

impl Default for IndexerFlags {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            enable_rss: true,
            enable_automatic_search: true,
            priority: default_priority(),
            minimum_seeders: 0,
        }
    }
}

/// Configuration for a [`Torznab`] indexer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorznabConfig {
    /// Display name (shown in lists; not used on the wire).
    pub name: String,
    /// Indexer root URL (`/api` is appended by the client).
    pub base_url: String,
    /// Newznab category ids to search (e.g. `[2000]` for movies).
    pub categories: Vec<u32>,
    /// Steady-state request rate cap (requests/minute). `None` ⇒ the gentle
    /// [`DEFAULT_RATE_PER_MINUTE`]; `Some(0)` ⇒ no limit (SKADI-T-0189).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_per_minute: Option<u32>,
    /// Sonarr's per-indexer flags (SKADI-T-0505).
    #[serde(flatten, default)]
    pub flags: IndexerFlags,
}

/// Configuration for a [`Prowlarr`] aggregate indexer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProwlarrConfig {
    /// Display name (shown in lists; not used on the wire).
    pub name: String,
    /// Prowlarr root URL (`/api/v1/search` is appended by the client).
    pub base_url: String,
    /// Newznab category ids to search (e.g. `[2000, 3030]` for movies + audiobooks).
    pub categories: Vec<u32>,
    /// Steady-state request rate cap (requests/minute). `None` ⇒ the gentle
    /// [`DEFAULT_RATE_PER_MINUTE`]; `Some(0)` ⇒ no limit (SKADI-T-0189).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_per_minute: Option<u32>,
    /// Sonarr's per-indexer flags (SKADI-T-0505).
    #[serde(flatten, default)]
    pub flags: IndexerFlags,
}

/// Configuration for the [`Knaben`] public meta-search (SKADI-T-0618).
///
/// No `base_url` in the common case and no api key: the endpoint is a fixed
/// public URL and the service is account-less. `base_url` exists only so a test
/// can point the client at a mock server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnabenConfig {
    /// Display name (shown in lists; not used on the wire).
    pub name: String,
    /// Newznab category ids to scope results to (e.g. `[2000, 5000]`). Applied
    /// client-side — Knaben's own category numbering is not Newznab's.
    pub categories: Vec<u32>,
    /// Override the API endpoint. `None` ⇒ [`KNABEN_API_URL`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Steady-state request rate cap (requests/minute). `None` ⇒
    /// [`KNABEN_RATE_PER_MINUTE`], deliberately gentler than the shared default:
    /// the API is one person's server, offered without authentication, and its
    /// only published limit is a request not to pin its CPU.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_per_minute: Option<u32>,
    /// Sonarr's per-indexer flags (SKADI-T-0505).
    #[serde(flatten, default)]
    pub flags: IndexerFlags,
}

/// Default request cap for [`KnabenConfig`] — half the shared default.
pub const KNABEN_RATE_PER_MINUTE: u32 = 30;

/// Configuration for a native [`CardigannIndexer`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardigannConfig {
    /// Display name (shown in lists).
    pub name: String,
    /// The catalog definition this indexer is an instance of (e.g. `thepiratebay`).
    pub definition_id: String,
    /// Non-secret per-definition setting overrides (`apiurl`, toggles, …). Secret
    /// settings (login password/cookie) are sealed separately (SKADI-T-0258).
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    /// Steady-state request rate cap (requests/minute). `None` ⇒ the gentle
    /// [`DEFAULT_RATE_PER_MINUTE`]; `Some(0)` ⇒ no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_per_minute: Option<u32>,
    /// Sonarr's per-indexer flags (SKADI-T-0505).
    #[serde(flatten, default)]
    pub flags: IndexerFlags,
}

/// Configuration for the built-in [`StubIndexer`] — just a display name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StubConfig {
    pub name: String,
}

impl IndexerConfig {
    /// Display name, regardless of kind.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            IndexerConfig::Torznab(c) => &c.name,
            IndexerConfig::Prowlarr(c) => &c.name,
            IndexerConfig::Knaben(c) => &c.name,
            IndexerConfig::Cardigann(c) => &c.name,
            IndexerConfig::Stub(c) => &c.name,
        }
    }

    /// The catalog definition id this indexer instantiates, if it's a cardigann
    /// indexer — the provider factory resolves it from the catalog and calls
    /// [`Self::build_cardigann`].
    #[must_use]
    pub fn cardigann_definition_id(&self) -> Option<&str> {
        match self {
            IndexerConfig::Cardigann(c) => Some(&c.definition_id),
            _ => None,
        }
    }

    /// Whether this indexer needs **no secret** — the built-in stub, and Knaben,
    /// which is an account-less public API (SKADI-T-0618). The provider factory
    /// branches on this to skip the credential lookup.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        matches!(self, IndexerConfig::Stub(_) | IndexerConfig::Knaben(_))
    }

    /// Validate and build the live indexer. `api_key` comes from the credential
    /// store (ignored for built-ins); `http` is the daemon's shared client.
    pub fn build(
        self,
        id: IndexerId,
        api_key: String,
        http: HttpClient,
    ) -> Result<Box<dyn Indexer>> {
        match self {
            IndexerConfig::Cardigann(_) => Err(AppError::Internal(
                "cardigann indexers are built via build_cardigann (needs a resolved definition)"
                    .into(),
            )),
            IndexerConfig::Stub(_) => Ok(indexer_health_tracked(Box::new(StubIndexer::new(id)))),
            IndexerConfig::Torznab(cfg) => {
                if cfg.base_url.trim().is_empty()
                    || !(cfg.base_url.starts_with("http://")
                        || cfg.base_url.starts_with("https://"))
                {
                    return Err(AppError::Validation(format!(
                        "torznab indexer {:?}: base_url must be an http(s) URL",
                        cfg.name
                    )));
                }
                if cfg.categories.is_empty() {
                    return Err(AppError::Validation(format!(
                        "torznab indexer {:?}: categories must be non-empty",
                        cfg.name
                    )));
                }
                let rate = cfg.rate_per_minute.unwrap_or(DEFAULT_RATE_PER_MINUTE);
                let categories = cfg.categories.into_iter().map(Category).collect();
                let inner = Box::new(
                    Torznab::new(id, cfg.base_url, api_key, categories, http).with_flags(cfg.flags),
                );
                Ok(indexer_health_tracked(indexer_rate_limited(inner, rate)))
            }
            IndexerConfig::Knaben(cfg) => {
                if cfg.categories.is_empty() {
                    return Err(AppError::Validation(format!(
                        "knaben indexer {:?}: categories must be non-empty",
                        cfg.name
                    )));
                }
                let base_url = cfg.base_url.unwrap_or_else(|| KNABEN_API_URL.to_string());
                if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
                    return Err(AppError::Validation(format!(
                        "knaben indexer {:?}: base_url must be an http(s) URL",
                        cfg.name
                    )));
                }
                let rate = cfg.rate_per_minute.unwrap_or(KNABEN_RATE_PER_MINUTE);
                let categories = cfg.categories.into_iter().map(Category).collect();
                let inner =
                    Box::new(Knaben::new(id, base_url, categories, http).with_flags(cfg.flags));
                Ok(indexer_health_tracked(indexer_rate_limited(inner, rate)))
            }
            IndexerConfig::Prowlarr(cfg) => {
                if cfg.base_url.trim().is_empty()
                    || !(cfg.base_url.starts_with("http://")
                        || cfg.base_url.starts_with("https://"))
                {
                    return Err(AppError::Validation(format!(
                        "prowlarr indexer {:?}: base_url must be an http(s) URL",
                        cfg.name
                    )));
                }
                if cfg.categories.is_empty() {
                    return Err(AppError::Validation(format!(
                        "prowlarr indexer {:?}: categories must be non-empty",
                        cfg.name
                    )));
                }
                let rate = cfg.rate_per_minute.unwrap_or(DEFAULT_RATE_PER_MINUTE);
                let categories = cfg.categories.into_iter().map(Category).collect();
                let inner = Box::new(
                    Prowlarr::new(id, cfg.base_url, api_key, categories, http)
                        .with_flags(cfg.flags),
                );
                Ok(indexer_health_tracked(indexer_rate_limited(inner, rate)))
            }
        }
    }

    /// Build a native cardigann indexer from its resolved `definition` (looked up
    /// from the catalog by the provider factory) + the shared client. Wrapped in
    /// the same rate-limit + health decorators as every other indexer.
    ///
    /// # Errors
    /// [`AppError::Internal`] if called on a non-cardigann config;
    /// [`AppError::Validation`] if the definition has no site link.
    pub fn build_cardigann(
        self,
        id: IndexerId,
        definition: Arc<Definition>,
        secret: Option<String>,
        flaresolverr_url: Option<&str>,
        proxy_url: Option<&str>,
    ) -> Result<Box<dyn Indexer>> {
        let IndexerConfig::Cardigann(cfg) = self else {
            return Err(AppError::Internal(
                "build_cardigann called on a non-cardigann config".into(),
            ));
        };
        if definition.links.is_empty() {
            return Err(AppError::Validation(format!(
                "cardigann indexer {:?}: definition '{}' has no site link",
                cfg.name, cfg.definition_id
            )));
        }
        let rate = cfg.rate_per_minute.unwrap_or(DEFAULT_RATE_PER_MINUTE);
        // The sealed secret is a JSON object of secret settings (login creds).
        let inner = Box::new(
            CardigannIndexer::new(
                id,
                definition,
                &cfg.settings,
                secret.as_deref(),
                flaresolverr_url,
                proxy_url,
            )
            .with_flags(cfg.flags),
        );
        Ok(indexer_health_tracked(indexer_rate_limited(inner, rate)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http() -> HttpClient {
        HttpClient::new(std::time::Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn deserializes_and_builds_torznab() {
        let json = serde_json::json!({
            "kind": "torznab",
            "name": "geek",
            "base_url": "https://api.nzbgeek.info",
            "categories": [2000, 2040]
        });
        let cfg: IndexerConfig = serde_json::from_value(json).unwrap();
        let built = cfg.build(IndexerId::new(), "key".into(), http());
        assert!(built.is_ok());
    }

    #[test]
    fn config_round_trips() {
        let cfg = IndexerConfig::Torznab(TorznabConfig {
            name: "geek".into(),
            base_url: "https://x".into(),
            categories: vec![2000],
            rate_per_minute: None,
            flags: Default::default(),
        });
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["kind"], "torznab");
        let back: IndexerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn rejects_bad_base_url_and_empty_categories() {
        let bad_url = IndexerConfig::Torznab(TorznabConfig {
            name: "x".into(),
            base_url: "not-a-url".into(),
            categories: vec![2000],
            rate_per_minute: None,
            flags: Default::default(),
        });
        assert!(matches!(
            bad_url.build(IndexerId::new(), "k".into(), http()),
            Err(AppError::Validation(_))
        ));

        let no_cats = IndexerConfig::Torznab(TorznabConfig {
            name: "x".into(),
            base_url: "https://ok".into(),
            categories: vec![],
            rate_per_minute: None,
            flags: Default::default(),
        });
        assert!(matches!(
            no_cats.build(IndexerId::new(), "k".into(), http()),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn unknown_kind_fails_to_deserialize() {
        let json = serde_json::json!({ "kind": "newznab", "name": "x" });
        assert!(serde_json::from_value::<IndexerConfig>(json).is_err());
    }

    /// Knaben is a public, account-less API, so it must build with an empty
    /// secret — the provider factory skips the credential lookup for it exactly
    /// as it does for the stub (SKADI-T-0618).
    #[test]
    fn knaben_needs_no_secret_and_defaults_its_endpoint() {
        let cfg = IndexerConfig::Knaben(KnabenConfig {
            name: "Knaben".into(),
            categories: vec![2000, 5000],
            base_url: None,
            rate_per_minute: None,
            flags: IndexerFlags::default(),
        });
        assert_eq!(cfg.name(), "Knaben");
        assert!(
            cfg.is_builtin(),
            "no credential lookup for an account-less API"
        );
        let http = HttpClient::new(std::time::Duration::from_secs(5)).unwrap();
        let built = cfg.build(IndexerId::new(), String::new(), http).unwrap();
        assert!(built.supports(skadi_core::MediaKind::Movie));
        assert!(built.supports(skadi_core::MediaKind::Series));
        assert!(!built.supports(skadi_core::MediaKind::Audiobook));
    }

    #[test]
    fn knaben_rejects_an_empty_category_list() {
        let cfg = IndexerConfig::Knaben(KnabenConfig {
            name: "Knaben".into(),
            categories: vec![],
            base_url: None,
            rate_per_minute: None,
            flags: IndexerFlags::default(),
        });
        let http = HttpClient::new(std::time::Duration::from_secs(5)).unwrap();
        assert!(cfg.build(IndexerId::new(), String::new(), http).is_err());
    }

    /// The stored shape is what an operator POSTs to `/settings/indexers`.
    #[test]
    fn knaben_round_trips_through_the_stored_shape() {
        let json = serde_json::json!({
            "kind": "knaben",
            "name": "Knaben",
            "categories": [2000, 5000, 3030],
            "enable_rss": false,
            "priority": 30
        });
        let cfg: IndexerConfig = serde_json::from_value(json).unwrap();
        let IndexerConfig::Knaben(k) = &cfg else {
            panic!("expected knaben, got {cfg:?}")
        };
        assert_eq!(k.categories, vec![2000, 5000, 3030]);
        assert_eq!(k.base_url, None);
        assert!(!k.flags.enable_rss);
        assert_eq!(k.flags.priority, 30);
        // And back out again without inventing fields.
        let back = serde_json::to_value(&cfg).unwrap();
        assert_eq!(back["kind"], "knaben");
        assert!(back.get("base_url").is_none());
        assert!(back.get("api_key").is_none());
    }

    #[test]
    fn stub_is_builtin_and_builds_without_a_secret() {
        let cfg: IndexerConfig =
            serde_json::from_value(serde_json::json!({ "kind": "stub", "name": "built-in" }))
                .unwrap();
        assert!(cfg.is_builtin());
        assert_eq!(cfg.name(), "built-in");
        // An empty api_key is fine for the built-in stub.
        assert!(cfg.build(IndexerId::new(), String::new(), http()).is_ok());
    }

    #[test]
    fn rate_per_minute_is_optional_and_round_trips() {
        // Absent in JSON → None (back-compat with pre-T-0189 stored configs).
        let cfg: IndexerConfig = serde_json::from_value(serde_json::json!({
            "kind": "prowlarr",
            "name": "p",
            "base_url": "https://ok",
            "categories": [2000]
        }))
        .unwrap();
        let IndexerConfig::Prowlarr(p) = &cfg else {
            panic!("expected prowlarr");
        };
        assert_eq!(p.rate_per_minute, None);
        // None is omitted from the serialized form (skip_serializing_if).
        let json = serde_json::to_value(&cfg).unwrap();
        assert!(json.get("rate_per_minute").is_none());

        // An explicit value round-trips.
        let cfg2: IndexerConfig = serde_json::from_value(serde_json::json!({
            "kind": "prowlarr",
            "name": "p",
            "base_url": "https://ok",
            "categories": [2000],
            "rate_per_minute": 30
        }))
        .unwrap();
        let IndexerConfig::Prowlarr(p2) = &cfg2 else {
            panic!("expected prowlarr");
        };
        assert_eq!(p2.rate_per_minute, Some(30));
        // Builds fine with an explicit cap (and with 0 = unlimited).
        assert!(cfg2.build(IndexerId::new(), "k".into(), http()).is_ok());
    }

    #[test]
    fn torznab_is_not_builtin() {
        let cfg = IndexerConfig::Torznab(TorznabConfig {
            name: "x".into(),
            base_url: "https://ok".into(),
            categories: vec![2000],
            rate_per_minute: None,
            flags: Default::default(),
        });
        assert!(!cfg.is_builtin());
    }

    const PUBLIC_DEF: &str = "id: thepiratebay\nname: TPB\ntype: public\nlinks: [https://thepiratebay.org/]\ncaps:\n  modes: {search: [q]}\nsearch:\n  rows:\n    selector: tr\n";

    #[test]
    fn cardigann_config_round_trips_and_reports_definition_id() {
        let json = serde_json::json!({
            "kind": "cardigann",
            "name": "TPB",
            "definition_id": "thepiratebay",
            "settings": { "apiurl": "apibay.org" }
        });
        let cfg: IndexerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.name(), "TPB");
        assert_eq!(cfg.cardigann_definition_id(), Some("thepiratebay"));
        assert!(!cfg.is_builtin());
        // round-trips through serde with the `cardigann` tag
        let back: IndexerConfig =
            serde_json::from_value(serde_json::to_value(&cfg).unwrap()).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn build_cardigann_builds_from_resolved_definition() {
        let cfg: IndexerConfig = serde_json::from_value(serde_json::json!({
            "kind": "cardigann",
            "name": "TPB",
            "definition_id": "thepiratebay"
        }))
        .unwrap();
        let def = Arc::new(skadi_cardigann::parse_definition(PUBLIC_DEF).unwrap());
        assert!(
            cfg.build_cardigann(IndexerId::new(), def, None, None, None)
                .is_ok()
        );
    }

    #[test]
    fn build_cardigann_rejects_definition_without_links() {
        let cfg = IndexerConfig::Cardigann(CardigannConfig {
            name: "x".into(),
            definition_id: "nolinks".into(),
            settings: BTreeMap::new(),
            rate_per_minute: None,
            flags: Default::default(),
        });
        let def = Arc::new(skadi_cardigann::parse_definition("id: nolinks\nname: N\n").unwrap());
        assert!(matches!(
            cfg.build_cardigann(IndexerId::new(), def, None, None, None),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn generic_build_refuses_cardigann() {
        let cfg = IndexerConfig::Cardigann(CardigannConfig {
            name: "x".into(),
            definition_id: "d".into(),
            settings: BTreeMap::new(),
            rate_per_minute: None,
            flags: Default::default(),
        });
        assert!(matches!(
            cfg.build(IndexerId::new(), String::new(), http()),
            Err(AppError::Internal(_))
        ));
    }
}

#[cfg(test)]
mod tag_scope_tests {
    use super::*;

    /// The regression that would break every existing install (SKADI-T-0556).
    #[test]
    fn an_untagged_indexer_applies_to_everything() {
        let flags = IndexerFlags::default();
        assert!(flags.tags.is_empty());
        assert!(flags.applies_to(&[]), "an untagged item");
        assert!(
            flags.applies_to(&["anime".into()]),
            "and a tagged one — an indexer that has not opted into scoping must \
             keep serving everything, or shipping this would silently empty the \
             fan-out on every upgrade"
        );
    }

    #[test]
    fn a_tagged_indexer_applies_only_where_a_tag_is_shared() {
        let flags = IndexerFlags {
            tags: vec!["anime".into(), "uhd".into()],
            ..IndexerFlags::default()
        };
        assert!(flags.applies_to(&["anime".into()]), "shares one");
        assert!(
            flags.applies_to(&["uhd".into(), "other".into()]),
            "shares one of several"
        );
        assert!(!flags.applies_to(&["other".into()]), "shares none");
        // An untagged *item* against a tagged indexer: no overlap, so no. This is
        // the direction that is easy to get backwards — the safe default belongs
        // on the indexer's empty set, not the item's.
        assert!(!flags.applies_to(&[]));
    }

    #[test]
    fn tags_default_to_empty_when_absent_from_stored_json() {
        // Every existing indexer row predates this field. Deserialising one must
        // yield "applies to everything", not a parse error and not an empty-means-
        // nothing indexer.
        let flags: IndexerFlags = serde_json::from_str("{}").expect("older rows still parse");
        assert!(flags.tags.is_empty());
        assert!(flags.applies_to(&["anything".into()]));
    }
}
