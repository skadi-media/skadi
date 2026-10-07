//! `skadi` — the command-line interface (SKADI-T-0056).
//!
//! Per the vision principle, every subcommand except `run` is a thin HTTP client
//! against a running daemon (via [`skadi_client`]). `run` is the daemon itself:
//! it loads [`Config`](skadi_api::Config), bootstraps the database, builds the
//! compiled-in domain registry (movies for v0), starts the per-domain supervisor
//! and the HTTP server, and awaits Ctrl-C for graceful shutdown.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;

use skadi_client::{ApiError, Client};

/// Skadi — a unified, Rust-native replacement for the *arr stack.
#[derive(Parser)]
#[command(name = "skadi", version, about)]
struct Cli {
    /// Daemon base URL for client subcommands (env: SKADI_API_URL).
    #[arg(
        long,
        global = true,
        env = "SKADI_API_URL",
        default_value = "http://127.0.0.1:8080"
    )]
    url: String,
    /// Bearer token for client subcommands (env: SKADI_API_TOKEN).
    #[arg(long, global = true, env = "SKADI_API_TOKEN")]
    token: Option<String>,
    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pretty: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the Skadi daemon (bootstrap + supervisor + HTTP server).
    Run,
    /// Print version information.
    Version,
    /// Check daemon health. Liveness by default; `--ready` for readiness, which
    /// is what a container healthcheck or deploy gate wants (SKADI-T-0479).
    Health {
        /// Probe readiness (database + providers published) instead of liveness.
        /// Exits non-zero until the daemon can actually do its job.
        #[arg(long)]
        ready: bool,
        /// Show the per-subsystem checks behind the verdict, rather than the
        /// verdict alone (SKADI-T-0469) — what you want when readiness is
        /// failing and you need to know which subsystem is the reason.
        #[arg(long, conflicts_with = "ready")]
        checks: bool,
    },
    /// Re-grade library rows whose quality was never assessed (SKADI-T-0399).
    ///
    /// Adoption used to record the quality ladder's lowest tier for any file
    /// whose name yielded no quality, so the upgrade sweep saw the whole library
    /// as below cutoff. This pass re-derives each imported row's quality from its
    /// file name and records `Unknown` where nothing can be derived. Reads the
    /// database directly, so run it with the daemon stopped; prints what it would
    /// do unless `--apply` is given.
    BackfillQuality {
        /// Write the changes (default: report only).
        #[arg(long)]
        apply: bool,
        /// Grade rows that are still `Unknown` by **probing the file**
        /// (SKADI-T-0528), rather than only re-reading file names.
        ///
        /// The adopted library carries skadi's own canonical paths, which have
        /// no quality tokens, so the name pass leaves all of it Unknown — and
        /// the upgrade sweep skips Unknown, so that library is invisible to it.
        /// Probing reads each file's header, which over NFS for ~20k files is
        /// minutes to hours, so it is opt-in rather than automatic.
        #[arg(long)]
        probe: bool,
    },
    /// Re-seal stored credentials under the current `SKADI_SECRET_KEY`
    /// (SKADI-T-0521).
    ///
    /// Rotating the key otherwise strands every credential: the rows stay sealed
    /// under the key that is gone, the provider loader warns and skips each one,
    /// and the only route back is re-entering every password by hand. Also seals
    /// plaintext rows written while no key was configured.
    ///
    /// Run with the daemon stopped — it reads the database directly. Pass the
    /// **previous** key as `--old-key`; omit it when the rows are plaintext.
    /// A row readable under neither key is left untouched and reported, because it
    /// may be sealed under a third key you still have.
    Rekey {
        /// The key the credentials are currently sealed under.
        #[arg(long)]
        old_key: Option<String>,
    },
    /// Enable/disable/list compiled-in domains.
    #[command(subcommand)]
    Domain(DomainCmd),
    /// Add a movie by TMDB id. Profile/root default to the first registered
    /// ones when omitted; a search starts immediately unless --no-search.
    AddMovie {
        #[arg(long)]
        tmdb_id: u64,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        root: Option<String>,
        /// Register only — don't kick off the acquire search right away.
        #[arg(long)]
        no_search: bool,
    },
    /// Movie library commands.
    #[command(subcommand)]
    Movies(MoviesCmd),
    /// Trigger a manual acquire for one edition.
    Acquire {
        movie_id: String,
        edition_id: String,
    },
    /// Add an audiobook by Audible ASIN. Profile/root default to the first
    /// registered ones when omitted; a search starts immediately unless
    /// --no-search.
    AddAudiobook {
        #[arg(long)]
        asin: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        root: Option<String>,
        /// Register only — don't kick off the acquire search right away.
        #[arg(long)]
        no_search: bool,
    },
    /// Audiobook library commands.
    #[command(subcommand)]
    Audiobooks(AudiobooksCmd),
    /// Trigger a manual acquire for one book file.
    AcquireBook { book_id: String, file_id: String },
    /// Show the unified library.
    Library {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        monitored: Option<bool>,
    },
    /// Show in-flight acquire runs.
    Activity,
    /// Settings CRUD (kinds: indexers, downloaders, profiles, notifiers, custom_formats).
    #[command(subcommand)]
    Settings(SettingsCmd),
    /// Television library commands (SKADI-T-0469).
    #[command(subcommand)]
    Series(SeriesCmd),
    /// Grab/import history, newest first.
    History {
        /// How many records to return.
        #[arg(long)]
        limit: Option<u32>,
        /// Show totals by event kind instead of the records.
        #[arg(long)]
        counts: bool,
    },
    /// Blocklist commands.
    #[command(subcommand)]
    Blocklist(BlocklistCmd),
    /// Everything monitored and still missing, across every domain.
    Wanted,
    /// Fan a search across every enabled indexer.
    SearchAll {
        /// Search text. Omitted, the daemon searches for what is wanted.
        query: Option<String>,
    },
}

#[derive(Subcommand)]
enum SeriesCmd {
    /// List series.
    List {
        #[arg(long)]
        monitored: Option<bool>,
    },
    /// Show one series (with seasons and episodes).
    Show { id: String },
    /// Remove a series (no file deletion).
    Rm { id: String },
}

#[derive(Subcommand)]
enum BlocklistCmd {
    /// List blocked releases, optionally for one acquirable.
    List {
        #[arg(long)]
        acquirable: Option<String>,
    },
    /// Block a release from an inline JSON body.
    Add { json: String },
    /// Unblock one entry by id.
    Rm { id: String },
    /// Clear the entire blocklist.
    ///
    /// Named rather than reachable by passing no ids: wiping every block an
    /// operator has accumulated should have to be asked for (SKADI-T-0473).
    Clear,
}

#[derive(Subcommand)]
enum DomainCmd {
    /// List domains and their enabled state.
    List,
    /// Enable a domain.
    Enable { name: String },
    /// Disable a domain.
    Disable { name: String },
}

#[derive(Subcommand)]
enum MoviesCmd {
    /// List movies.
    List {
        #[arg(long)]
        monitored: Option<bool>,
    },
    /// Show one movie (with editions).
    Show { id: String },
    /// Remove a movie (no file deletion).
    Rm { id: String },
}

#[derive(Subcommand)]
enum AudiobooksCmd {
    /// List audiobooks.
    List {
        #[arg(long)]
        monitored: Option<bool>,
    },
    /// Show one audiobook (with files).
    Show { id: String },
    /// Remove an audiobook (no file deletion).
    Rm { id: String },
    /// Fold duplicate books rows into one book with several editions.
    ///
    /// Reports and changes nothing unless `--apply` is given (SKADI-T-0562).
    MergeEditions {
        /// Perform the merge. Without this the command is a dry run.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// List entities of a kind.
    List { kind: String },
    /// Create an entity from an inline JSON body.
    Add { kind: String, json: String },
    /// Replace an entity from an inline JSON body (SKADI-T-0469).
    Update {
        kind: String,
        id: String,
        json: String,
    },
    /// Delete an entity by id.
    Rm { kind: String, id: String },
    /// Test a provider's connectivity/credentials (indexers, downloaders, notifiers).
    Test { kind: String, id: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Capture recent lines in memory alongside stdout, so `GET /log` can answer
    // "what just happened" without shelling into the host (SKADI-T-0467). The
    // ring is bounded and never replaces stdout — container logs stay the record.
    let logs = skadi_api::logbuf::LogBuffer::new();
    {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with(tracing_subscriber::fmt::layer())
            .with(logs.layer())
            .init();
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move { dispatch(cli, logs).await })
}

/// Build an egress client honouring `http.proxy_url` / `http.no_proxy`
/// (SKADI-T-0522), so Torznab, metadata, notifier and definition-sync traffic
/// rides the VPN alongside cardigann's — which already built its own proxied
/// client, leaving everything else direct.
fn egress_client(
    view: &skadi_config::ConfigView,
    timeout: std::time::Duration,
    what: &'static str,
) -> anyhow::Result<skadi_http::HttpClient> {
    let (proxy, no_proxy) = skadi_config::egress_proxy(view);
    match proxy {
        Some(url) => skadi_http::HttpClient::with_proxy(
            timeout,
            skadi_http::RetryConfig::default(),
            &url,
            &no_proxy,
        )
        .with_context(|| format!("{what} (via proxy {url})")),
        None => skadi_http::HttpClient::new(timeout).context(what),
    }
}

async fn dispatch(cli: Cli, logs: skadi_api::logbuf::LogBuffer) -> Result<()> {
    let pretty = cli.pretty;
    let client = || Client::new(cli.url.clone(), cli.token.clone());

    match cli.command {
        Some(Commands::Run) => run_daemon(logs).await,
        Some(Commands::Version) | None => {
            println!("skadi {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Commands::Health { ready, checks }) => {
            let c = client();
            let r = if checks {
                c.health_checks().await
            } else if ready {
                c.readiness().await
            } else {
                c.health().await
            };
            print_result(r, pretty)
        }
        Some(Commands::BackfillQuality { apply, probe }) => backfill_quality(apply, probe).await,
        Some(Commands::Rekey { old_key }) => rekey(old_key.as_deref()).await,
        Some(Commands::Domain(DomainCmd::List)) => print_result(client().domains().await, pretty),
        Some(Commands::Domain(DomainCmd::Enable { name })) => {
            print_result(client().set_domain_enabled(&name, true).await, pretty)
        }
        Some(Commands::Domain(DomainCmd::Disable { name })) => {
            print_result(client().set_domain_enabled(&name, false).await, pretty)
        }
        Some(Commands::AddMovie {
            tmdb_id,
            profile,
            root,
            no_search,
        }) => print_result(
            client()
                .add_movie(tmdb_id, profile.as_deref(), root.as_deref(), !no_search)
                .await,
            pretty,
        ),
        Some(Commands::Movies(MoviesCmd::List { monitored })) => {
            print_result(client().list_movies(monitored).await, pretty)
        }
        Some(Commands::Movies(MoviesCmd::Show { id })) => {
            print_result(client().get_movie(&id).await, pretty)
        }
        Some(Commands::Movies(MoviesCmd::Rm { id })) => {
            print_result(client().delete_movie(&id).await, pretty)
        }
        Some(Commands::Acquire {
            movie_id,
            edition_id,
        }) => print_result(
            client().acquire_edition(&movie_id, &edition_id).await,
            pretty,
        ),
        Some(Commands::AddAudiobook {
            asin,
            profile,
            root,
            no_search,
        }) => print_result(
            client()
                .add_audiobook(&asin, profile.as_deref(), root.as_deref(), !no_search)
                .await,
            pretty,
        ),
        Some(Commands::Audiobooks(AudiobooksCmd::List { monitored })) => {
            print_result(client().list_audiobooks(monitored).await, pretty)
        }
        Some(Commands::Audiobooks(AudiobooksCmd::Show { id })) => {
            print_result(client().get_audiobook(&id).await, pretty)
        }
        Some(Commands::Audiobooks(AudiobooksCmd::Rm { id })) => {
            print_result(client().delete_audiobook(&id).await, pretty)
        }
        Some(Commands::Audiobooks(AudiobooksCmd::MergeEditions { apply })) => {
            print_result(client().merge_book_editions(apply).await, pretty)
        }
        Some(Commands::AcquireBook { book_id, file_id }) => {
            print_result(client().acquire_book_file(&book_id, &file_id).await, pretty)
        }
        Some(Commands::Library { kind, monitored }) => {
            print_result(client().library(kind.as_deref(), monitored).await, pretty)
        }
        Some(Commands::Activity) => print_result(client().activity().await, pretty),
        Some(Commands::Settings(SettingsCmd::List { kind })) => {
            print_result(client().list_settings(&kind).await, pretty)
        }
        Some(Commands::Settings(SettingsCmd::Add { kind, json })) => {
            let body: Value = serde_json::from_str(&json).context("body must be valid JSON")?;
            print_result(client().create_setting(&kind, &body).await, pretty)
        }
        Some(Commands::Settings(SettingsCmd::Update { kind, id, json })) => {
            let body: Value = serde_json::from_str(&json).context("body must be valid JSON")?;
            print_result(client().update_setting(&kind, &id, &body).await, pretty)
        }
        Some(Commands::Series(SeriesCmd::List { monitored })) => {
            print_result(client().list_series(monitored).await, pretty)
        }
        Some(Commands::Series(SeriesCmd::Show { id })) => {
            print_result(client().get_series(&id).await, pretty)
        }
        Some(Commands::Series(SeriesCmd::Rm { id })) => {
            print_result(client().delete_series(&id).await, pretty)
        }
        Some(Commands::History { limit, counts }) => {
            let c = client();
            let r = if counts {
                c.history_counts().await
            } else {
                c.history(limit).await
            };
            print_result(r, pretty)
        }
        Some(Commands::Blocklist(BlocklistCmd::List { acquirable })) => {
            print_result(client().list_blocklist(acquirable.as_deref()).await, pretty)
        }
        Some(Commands::Blocklist(BlocklistCmd::Add { json })) => {
            let body: Value = serde_json::from_str(&json).context("body must be valid JSON")?;
            print_result(client().block_release(&body).await, pretty)
        }
        Some(Commands::Blocklist(BlocklistCmd::Rm { id })) => {
            print_result(client().unblock_release(&id).await, pretty)
        }
        Some(Commands::Blocklist(BlocklistCmd::Clear)) => {
            print_result(client().clear_blocklist().await, pretty)
        }
        Some(Commands::Wanted) => print_result(client().wanted().await, pretty),
        Some(Commands::SearchAll { query }) => {
            print_result(client().search_all(query.as_deref()).await, pretty)
        }
        Some(Commands::Settings(SettingsCmd::Rm { kind, id })) => {
            print_result(client().delete_setting(&kind, &id).await, pretty)
        }
        Some(Commands::Settings(SettingsCmd::Test { kind, id })) => {
            let result = client().test_setting(&kind, &id).await;
            // Surface a failed test as a non-zero exit, not just `ok:false` JSON.
            let failed = matches!(&result, Ok(v) if v["ok"] == false);
            print_result(result, pretty)?;
            if failed {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

/// Print a client result as JSON, or report the error and exit non-zero.
fn print_result(result: std::result::Result<Value, ApiError>, pretty: bool) -> Result<()> {
    match result {
        Ok(value) => {
            let s = if pretty {
                serde_json::to_string_pretty(&value)?
            } else {
                serde_json::to_string(&value)?
            };
            println!("{s}");
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

// --- daemon ---

/// Resolve when the process is asked to stop, returning which signal did it.
///
/// SIGTERM is what an orchestrator sends first (`docker stop`, systemd,
/// Kubernetes); Ctrl-C is what a developer sends. Both mean the same thing here.
/// On non-Unix only Ctrl-C exists.
async fn wait_for_shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "cannot listen for SIGTERM; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return "SIGINT";
            }
        };
        tokio::select! {
            _ = term.recv() => "SIGTERM",
            r = tokio::signal::ctrl_c() => {
                if r.is_err() {
                    // Never resolve on a broken handler rather than reporting a
                    // shutdown nobody asked for.
                    std::future::pending::<()>().await;
                }
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}

async fn run_daemon(logs: skadi_api::logbuf::LogBuffer) -> Result<()> {
    use std::time::Duration;

    use skadi_api::{AppState, Config, DEFAULT_TICK_INTERVAL, DomainDescriptor, Supervisor};
    use skadi_audiobooks::{
        AudiobooksHttp, AudiobooksModule, SharedHunterDeps as AbDeps, default_audiobook_profile,
    };
    use skadi_core::{DomainModule, MediaKind};
    use skadi_hunter::services::AudiobookScoring;
    use skadi_hunter::services::ScoringConfig;
    use skadi_metadata::TmdbProvider;
    use skadi_movies::{MoviesHttp, MoviesLibrary, MoviesModule, SharedHunterDeps};
    use skadi_quality::audiobook::default_audiobook_definitions;
    use skadi_quality::{default_definitions, standard_profile};
    use skadi_store::Store;
    use skadi_tv::{
        SharedHunterDeps as TvDeps, TelevisionHttp, TelevisionLibrary, TelevisionModule,
    };

    // Tier-0 (database_url) + initial bind/token from env; the table-backed
    // fields are re-resolved from the `config` table after bootstrap seeds it.
    let mut config = Config::from_env().context("loading config")?;

    // The no-config fallback is the Standard profile (720p+ only, cutoff
    // Bluray-1080p, upgrades on) — NOT permissive: with zero configuration
    // Skadi must never grab SDTV/DVD junk (SKADI-I-0012).
    let defs = default_definitions();
    let profile = standard_profile(&defs);

    // Minimum seeders for a torrent release to be eligible in `decide`
    // (SKADI-T-0113). This is a config-plane key; at boot the env is the
    // authority (it's what bootstrap seeds into the `config` table), so read it
    // here, falling back to the registry default.
    let min_seeders: u32 = std::env::var(skadi_config::env_name("min_seeders"))
        .ok()
        .or_else(|| {
            skadi_config::default_for("min_seeders")
                .ok()
                .map(str::to_owned)
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    // Providers start empty here; the supervisor's first reconcile (below)
    // builds the live set from stored settings + sealed credentials and
    // publishes it — and re-publishes whenever provider settings change
    // (SKADI-I-0009). No restart needed to reconfigure.
    let shared = SharedHunterDeps {
        indexers: vec![],
        downloaders: vec![],
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: defs,
            profile,
            formats: vec![],
            min_seeders,
            audiobook: None,
        },
    };

    // The movies module's Cloacina runner connects to the `cloacina` database at
    // construction (Postgres) or a sibling file (SQLite), so ensure the Postgres
    // database exists before building the module. Idempotent; no-op on SQLite.
    skadi_api::ensure_cloacina_database(&config.database_url)
        .await
        .context("ensure cloacina database")?;

    // Build ONE Cloacina runner shared by every domain module (SKADI-T-0136):
    // all domains dispatch their acquire workflows through a single runner /
    // `hunter.db` (keyed by `MediaKind` in the per-kind HunterServices registry)
    // instead of one runner per module colliding on the same database.
    let shared_runner = Arc::new(
        skadi_hunter::build_runner(&config.database_url)
            .await
            .context("building shared cloacina runner")?,
    );

    // Build the movies module against the shared runner and the registry.
    let module_store = Store::connect(&config.database_url).context("connect store")?;
    let module = Arc::new(
        MoviesModule::with_runner(module_store, shared_runner.clone(), shared)
            .await
            .context("building movies module")?,
    );
    // Build the television module against the SAME shared runner. Standard video
    // profile (like movies); providers fill on the first reconcile. TV metadata is
    // the keyless Skyhook provider, set below.
    let tv_defs = default_definitions();
    let tv_profile = standard_profile(&tv_defs);
    let tv_shared = TvDeps {
        indexers: vec![],
        downloaders: vec![],
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: tv_defs,
            profile: tv_profile,
            formats: vec![],
            min_seeders,
            audiobook: None,
        },
    };
    let tv_store = Store::connect(&config.database_url).context("connect store")?;
    let television = Arc::new(
        TelevisionModule::with_runner(tv_store, shared_runner.clone(), tv_shared)
            .await
            .context("building television module")?,
    );

    // Build the audiobooks module against the SAME shared runner (SKADI-T-0129,
    // SKADI-T-0136). Its scoring carries the audiobook quality axis
    // (`audiobook: Some(..)`); providers start empty and are filled by the
    // supervisor's first reconcile, exactly like movies.
    let ab_shared = AbDeps {
        indexers: vec![],
        downloaders: vec![],
        notifiers: vec![],
        scoring: ScoringConfig {
            definitions: vec![],
            profile: default_audiobook_profile(),
            formats: vec![],
            min_seeders,
            audiobook: Some(AudiobookScoring {
                definitions: default_audiobook_definitions(),
                allow_abridged: false,
            }),
        },
    };
    let ab_store = Store::connect(&config.database_url).context("connect store")?;
    let audiobooks = Arc::new(
        AudiobooksModule::with_runner(ab_store, shared_runner.clone(), ab_shared)
            .await
            .context("building audiobooks module")?,
    );

    let registry: Vec<Arc<dyn DomainModule>> =
        vec![module.clone(), television.clone(), audiobooks.clone()];

    // Bootstrap: run store + domain migrations, seed the domains table (and
    // re-ensure the cloacina db, idempotently).
    skadi_api::bootstrap(&config, &registry)
        .await
        .context("bootstrap")?;

    // The shared store for AppState + supervisor (bootstrap used its own).
    let store = Store::connect(&config.database_url).context("connect store")?;

    // The config table is now seeded; resolve the table-backed runtime fields
    // (bind_addr, api_token) from it, and read the rest of process config
    // (tmdb_api_key) through the same view (SKADI-I-0014).
    let config_view = skadi_api::load_config_view(&store)
        .await
        .context("load config view")?;
    config
        .resolve(&config_view)
        .context("resolving config from the config table")?;

    // The movies HTTP routes + library provider share the module's runner/store.
    //
    // Metadata source selection (ADR SKADI-A-0001): a TMDB API key, when set,
    // selects the sanctioned TMDB path; otherwise default to Servarr's keyless
    // public metadata API — acceptable only while this project is private; see
    // the ADR's review triggers before publishing or if lookups start failing.
    let http = egress_client(&config_view, Duration::from_secs(30), "http client")?;
    let tmdb_key = config_view
        .get_opt_string("tmdb_api_key")
        .context("reading tmdb_api_key")?;
    let provider: Arc<dyn skadi_metadata::MetadataProvider> = match tmdb_key.clone() {
        Some(key) => {
            tracing::info!("metadata provider: tmdb (tmdb_api_key set)");
            Arc::new(TmdbProvider::new(key, http.clone()))
        }
        None => {
            tracing::info!("metadata provider: servarr (api.radarr.video, keyless)");
            Arc::new(skadi_metadata::ServarrProvider::new(http.clone()))
        }
    };
    // Warm the metadata provider's HTTP client in the background so the first
    // real lookup (e.g. a library-import match) doesn't eat the DNS + TLS
    // cold-start (~1-2 min on a fresh container). Best-effort: retry a few times,
    // ignore the result (SKADI-T-0078).
    {
        let provider = provider.clone();
        tokio::spawn(async move {
            for attempt in 0..3u32 {
                let q = skadi_metadata::MetadataQuery {
                    title: "matrix".into(),
                    year: None,
                    kind: MediaKind::Movie,
                };
                match provider.search(&q).await {
                    Ok(_) => {
                        tracing::info!("metadata provider warmed");
                        return;
                    }
                    Err(e) => {
                        tracing::debug!(attempt, error = %e, "metadata warmup attempt failed");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        });
    }

    // Hand the provider to the module so its periodic metadata-refresh worker
    // (SKADI-T-0041) can run; the supervisor spawns workers() after this.
    module.set_metadata_provider(provider.clone());

    // Television metadata is the keyless Skyhook provider (TheTVDB) — TV is
    // tvdb-native, exactly how Sonarr works. Drives the refresh worker + the HTTP
    // add/lookup handlers.
    let tv_http_client =
        egress_client(&config_view, Duration::from_secs(30), "skyhook http client")?;
    let tv_provider: Arc<dyn skadi_metadata::SeriesMetadataProvider> =
        Arc::new(skadi_metadata::SkyhookProvider::new(tv_http_client));
    television.set_series_provider(tv_provider.clone());

    // Audiobooks metadata source is Audnexus (keyless, ASIN-keyed; SKADI-I-0017).
    // Setting it drives the metadata-refresh worker (SKADI-T-0135) and, paired
    // with the catalog provider below, author-discovery (SKADI-T-0132).
    let ab_http = egress_client(
        &config_view,
        Duration::from_secs(30),
        "audnexus http client",
    )?;
    // Keep a concrete `Arc<AudnexusProvider>` for the audiobooks HTTP surface
    // (its author lookups call inherent methods not on the generic trait), and
    // clone-coerce one copy into the module's `set_metadata_provider` slot.
    let ab_provider_concrete = Arc::new(skadi_metadata::AudnexusProvider::new(ab_http));
    let ab_provider: Arc<dyn skadi_metadata::MetadataProvider> = ab_provider_concrete.clone();
    audiobooks.set_metadata_provider(ab_provider);

    // Author-monitoring discovery (SKADI-T-0132): the Audible catalog API lists a
    // monitored author's products (Audnexus has no author-catalog endpoint); each
    // new ASIN is enriched via the Audnexus provider above. Once both are wired,
    // the module spawns its author-discovery worker.
    let ab_catalog_http = egress_client(
        &config_view,
        Duration::from_secs(30),
        "audible catalog http client",
    )?;
    let ab_catalog = Arc::new(skadi_metadata::AudibleCatalogProvider::new(ab_catalog_http));
    audiobooks.set_catalog_provider(ab_catalog.clone());
    let audiobooks_http = AudiobooksHttp::new(
        audiobooks.store(),
        ab_provider_concrete,
        Some(ab_catalog),
        audiobooks.runner(),
    );

    let movies_http = MoviesHttp::new(module.store(), provider.clone(), module.runner());
    // `with_provider` is what makes movies addable from an import list
    // (SKADI-T-0511): the hook goes through `add_movie`, which needs metadata.
    let movies_library: Arc<dyn skadi_api::LibraryProvider> =
        Arc::new(MoviesLibrary::new(module.store()).with_provider(provider.clone()));

    let television_http =
        TelevisionHttp::new(television.store(), tv_provider.clone(), television.runner());
    // `with_provider` is what makes series addable from an import list
    // (SKADI-T-0564); the hook goes through `add_series`, which needs metadata.
    let television_library: Arc<dyn skadi_api::LibraryProvider> =
        Arc::new(TelevisionLibrary::new(television.store()).with_provider(tv_provider.clone()));

    // Audiobooks contributes to the unified library too (SKADI-T-0423). It
    // registered no provider before, so /library silently omitted every book and
    // `unmapped` reported every author folder as free.
    let audiobooks_library: Arc<dyn skadi_api::LibraryProvider> =
        Arc::new(skadi_audiobooks::AudiobooksLibrary::new(audiobooks.store()));

    let mut state = AppState::new_full(
        config.clone(),
        Some(store.clone()),
        vec![
            DomainDescriptor {
                name: "movies".into(),
                kind: MediaKind::Movie,
            },
            DomainDescriptor {
                name: "television".into(),
                kind: MediaKind::Series,
            },
            DomainDescriptor {
                name: "audiobooks".into(),
                kind: MediaKind::Audiobook,
            },
        ],
        vec![movies_library, television_library, audiobooks_library],
    );
    // Serve the captured lines from `GET /log` (SKADI-T-0467). `new_full` makes
    // its own empty ring; swap in the one the subscriber is actually feeding.
    Arc::get_mut(&mut state)
        .expect("AppState not yet shared")
        .logs = logs;
    // The domains migrate into the store's database; the `database` health
    // check compares the applied schema with the store's AND these sets
    // (SKADI-T-0682).
    Arc::get_mut(&mut state)
        .expect("AppState not yet shared")
        .domain_migrations = skadi_api::bootstrap::domain_migration_versions(&store, &registry)
        .context("read the domain migration versions")?
        .into();

    // Import-list providers (SKADI-T-0511, SKADI-T-0564).
    {
        use skadi_metadata::arr_list::ArrInstanceProvider;
        use skadi_metadata::import_list::{ImportListProvider, TmdbListProvider};
        let mut providers: Vec<Arc<dyn ImportListProvider>> = Vec::new();
        // TMDB's list and collection endpoints both need a daemon-wide API key,
        // so without one those two are absent and a configured TMDB list fails
        // with *that* reason rather than syncing nothing.
        if let Some(key) = tmdb_key.clone() {
            providers.push(Arc::new(TmdbListProvider::collections(
                key.clone(),
                http.clone(),
            )));
            providers.push(Arc::new(TmdbListProvider::lists(key, http.clone())));
        } else {
            tracing::info!("import lists: no tmdb_api_key, so no TMDB list providers");
        }
        // The *arr providers carry their credentials per list, in the list's own
        // settings, so they are always available (SKADI-T-0564). This is the
        // migration path: point skadi at an existing Radarr/Sonarr and it adopts
        // that library.
        providers.push(Arc::new(ArrInstanceProvider::radarr(http.clone())));
        providers.push(Arc::new(ArrInstanceProvider::sonarr(http.clone())));
        tracing::info!(
            providers = providers.len(),
            "import list providers registered"
        );
        Arc::get_mut(&mut state)
            .expect("AppState not yet shared")
            .import_list_providers = providers;
    }
    let state = state;

    // Supervisor reconciles domain workers AND the live provider set (from
    // settings + credentials, on its tick) on its own task, sharing the daemon
    // shutdown token. The movies module is its provider reloader: a settings
    // change re-publishes HunterServices without a restart.
    // Background: keep the native cardigann definition library fresh from
    // upstream on a schedule (SKADI-T-0263), like Prowlarr's daily refresh.
    // Best-effort; shares the daemon shutdown token.
    // Import lists sync on their own schedule (SKADI-T-0511); each list carries
    // its own interval and the worker only wakes to check what is due.
    tokio::spawn(skadi_api::import_lists::sync_worker(
        state.clone(),
        state.cancel.clone(),
    ));
    tokio::spawn(skadi_api::definitions::sync_loop(
        store.clone(),
        state.cancel.clone(),
    ));
    // Scheduled configuration backups (SKADI-T-0463). Off unless
    // `backup.interval_secs` is configured; shares the daemon shutdown token.
    tokio::spawn(skadi_api::backup::backup_loop(
        store.clone(),
        state.cancel.clone(),
    ));
    // Recycle-bin retention (SKADI-T-0418). Off unless both `import.recycle_bin`
    // and a non-zero `import.recycle_retention_days` are configured.
    tokio::spawn(skadi_api::backup::recycle_sweep_loop(
        store.clone(),
        state.cancel.clone(),
    ));
    // Abandoned upload sessions (SKADI-T-0629). A half-sent file sits on the
    // library mount until someone resumes it; this is what stops "until
    // someone resumes it" from meaning "forever".
    tokio::spawn(skadi_api::uploads::session_sweep_loop(
        state.clone(),
        state.cancel.clone(),
    ));

    let reloaders: Vec<Arc<dyn skadi_api::ProviderReloader>> =
        vec![module.clone(), television.clone(), audiobooks.clone()];
    let supervisor = Arc::new(
        Supervisor::with_reloaders(store, registry, reloaders)
            .with_readiness(state.providers_ready.clone())
            .with_failure_counts(state.worker_failures.clone())
            .with_live_token(state.clone()),
    );
    let sup_cancel = state.cancel.clone();
    let sup_handle = tokio::spawn(supervisor.run(sup_cancel, DEFAULT_TICK_INTERVAL));

    // Ctrl-C **or** SIGTERM cancels everything (SKADI-T-0476). Handling only
    // ctrl_c meant `docker stop` — which sends SIGTERM — was ignored, so every
    // stop ran to the kill timeout and ended in SIGKILL. That is how a container
    // gets torn down mid-write, and on 2026-09-07 it left a worker container with
    // an unkillable zombie PID.
    let sig_cancel = state.cancel.clone();
    tokio::spawn(async move {
        let reason = wait_for_shutdown_signal().await;
        tracing::info!(signal = reason, "shutdown signal received");
        sig_cancel.cancel();
    });

    tracing::info!("starting skadi daemon");
    // Domains hand up their response schemas alongside their routes so
    // `/openapi.json` can describe payloads, not just paths (SKADI-T-0546).
    let mut domain_schemas = skadi_api::HttpModule::schemas(&movies_http);
    domain_schemas.extend(skadi_api::HttpModule::schemas(&television_http));
    domain_schemas.extend(skadi_api::HttpModule::schemas(&audiobooks_http));
    skadi_api::serve::serve_with_schemas(
        state,
        vec![
            skadi_api::HttpModule::routes(&movies_http),
            skadi_api::HttpModule::routes(&television_http),
            skadi_api::HttpModule::routes(&audiobooks_http),
        ],
        domain_schemas,
    )
    .await
    .context("serving")?;
    let _ = sup_handle.await;
    Ok(())
}

/// `skadi backfill-quality` — re-grade rows whose quality was never assessed
/// (SKADI-T-0399). Talks to the database directly (no daemon), so the operator
/// runs it with the stack stopped; `--apply` writes, otherwise it only reports.
/// `skadi rekey` (SKADI-T-0521).
async fn rekey(old_key: Option<&str>) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let config = skadi_api::Config::from_env().context("load config")?;
    let store = skadi_store::Store::connect(&config.database_url).context("connect store")?;
    store.run_migrations().await.context("run migrations")?;

    let report = store.rekey_credentials(old_key).await.context("rekey")?;
    println!(
        "rekeyed {} credential(s); {} already current; {} unreadable",
        report.rekeyed, report.already_current, report.unreadable
    );
    if report.unreadable > 0 {
        // Not an error: the rows are intact and may be recoverable with another
        // key. But it must not read as a clean success either.
        eprintln!(
            "warning: {} credential(s) could not be read under the old or current key \
             and were left untouched — re-run with the right --old-key, or re-enter them",
            report.unreadable
        );
    }
    Ok(())
}

async fn backfill_quality(apply: bool, probe: bool) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let config = skadi_api::Config::from_env().context("load config")?;
    let store = skadi_store::Store::connect(&config.database_url).context("connect store")?;
    store.run_migrations().await.context("run migrations")?;

    let movies = skadi_movies::maintenance::backfill_unassessed_quality(&store, apply)
        .await
        .context("movies backfill")?;
    let tv = skadi_tv::maintenance::backfill_unassessed_quality(&store, apply)
        .await
        .context("television backfill")?;

    let probed = if probe {
        // Runs *after* the name pass, on whatever it left Unknown. A row the
        // name pass graded has real evidence behind it; overwriting that with an
        // assumed source would be a downgrade in confidence.
        let prober = skadi_media_probe::DefaultProber;
        let defs = skadi_quality::default_definitions();
        let movies =
            skadi_movies::maintenance::grade_unknown_by_probe(&store, &prober, &defs, apply)
                .await
                .context("movies probe grading")?;
        let tv = skadi_tv::maintenance::grade_unknown_by_probe(&store, &prober, &defs, apply)
            .await
            .context("television probe grading")?;
        Some((movies, tv))
    } else {
        None
    };

    let mode = if apply { "applied" } else { "dry run" };
    println!("quality backfill ({mode})");
    println!(
        "  movies:      scanned {:>6}  re-graded {:>6}  unknown {:>6}  kept {:>6}",
        movies.scanned, movies.graded, movies.unknown, movies.kept
    );
    println!(
        "  television:  scanned {:>6}  re-graded {:>6}  unknown {:>6}  kept {:>6}",
        tv.scanned, tv.graded, tv.unknown, tv.kept
    );
    if let Some((pm, pt)) = probed {
        println!(
            "  probe movies:     scanned {:>6}  graded {:>6}  unknown {:>6}  kept {:>6}",
            pm.scanned, pm.graded, pm.unknown, pm.kept
        );
        println!(
            "  probe television: scanned {:>6}  graded {:>6}  unknown {:>6}  kept {:>6}",
            pt.scanned, pt.graded, pt.unknown, pt.kept
        );
    } else {
        println!("\n(name-based only; pass --probe to grade the adopted library)");
    }
    if !apply {
        println!("\nnothing was written; re-run with --apply");
    }
    Ok(())
}
