# Skadi

A unified, Rust-native replacement for the \*arr stack. One daemon, one database,
one binary. Movies, TV, music, books, subtitles — all coordinated through a
shared library brain instead of five federated services that drift from each
other.

## Status

Design phase. Day-one target: movies. Subsequent media domains plug in via the
same `LibraryItem` / `Acquirable` traits without changes to core infrastructure.

License: Apache-2.0 OR MIT (standard Rust dual license).

## Principles

1. **Runtime state lives in the database.** Not in config files. Config-on-disk
   is a serialization format for backup, seeding, and disaster recovery — not
   the operational mechanism.
2. **All domains ship in the binary.** Enable/disable is a DB-backed runtime
   toggle, mutated via the UI. Compile-time Cargo features exist for slim
   builds but aren't the user-facing surface.
3. **Greenfield API.** No attempt at \*arr REST compatibility. We provide
   one-shot conversion utilities; users migrate by import, not by impersonation.
4. **Dual-backend storage from day one.** SQLite for instant first-run, Postgres
   for serious deployments, both behind one repository surface.
5. **Steal what \*arr did well.** Quality definitions, custom format scoring,
   release-title parsing — these encode years of community-tuned wisdom. We
   port the patterns; we own the implementation and the test corpus.
6. **The HTTP API is the only interface.** The bundled UI has no special
   access. CLI, web UI, and future TUIs / mobile apps are all peers consuming
   the same surface.

## Workspace

```
skadi/
├── Cargo.toml                  # workspace
├── crates/
│   ├── skadi-core              # LibraryItem/Acquirable traits, IDs, errors, state machine
│   ├── skadi-store             # Store, Diesel models, sqlite + postgres backends, migrations
│   ├── skadi-quality           # QualityProfile, CustomFormat, release-title parser
│   ├── skadi-metadata          # MetadataProvider trait + TMDB / TVDB / MusicBrainz impls
│   ├── skadi-indexers          # Indexer trait + Torznab / Newznab / RSS
│   ├── skadi-downloaders       # Downloader trait + clients (as built: one built-in client)
│   ├── skadi-importer          # parse → match → rename → hardlink/move
│   ├── skadi-hunter            # orchestration: Wanted → Search → Snatch → Download → Import → Notify
│   ├── skadi-notify            # Notifier trait + webhook / Discord / email / Pushover
│   ├── skadi-api               # axum router; mounts enabled domains; serves embedded UI
│   ├── skadi-client            # typed API client (shared by CLI and UI)
│   ├── skadi-ui                # Dioxus web SPA
│   ├── skadi-movies            # day-one domain module
│   ├── skadi-tv                # deferred
│   ├── skadi-music             # deferred
│   ├── skadi-books             # deferred
│   └── skadi-cli               # the binary; clap; wires the daemon and CLI client
├── migrations/
│   ├── sqlite/
│   └── postgres/
└── docs/
    └── design.md
```

Dependency direction is strictly downward. Domain modules
(`skadi-movies`, `skadi-tv`, ...) depend on core / store / quality / metadata /
indexers / downloaders / importer / hunter / notify, never on each other.
`skadi-api` mounts whichever domain modules registered themselves at startup.
`skadi-cli` is the binary; everything else is a library.

`skadi-client` is split out so that both the CLI and the web UI consume the
same typed client — there's one source of truth for API request/response
shapes.

## Core Domain Model

Two-level abstraction:

- **`LibraryItem`** — what a user actively tracks. Movies, series, artists,
  albums, books. One row in the user's mental model.
- **`Acquirable`** — what a `Release` actually satisfies. The unit a downloader
  hands to the importer. For movies this is a `MovieEdition`; for TV an
  `Episode`; for music a `Track` or `Album`.

```rust
// skadi-core/src/lib.rs

/// A thing the user actively wants in their library.
pub trait LibraryItem: Send + Sync {
    type Id: ItemId;
    type Acquirable: Acquirable<Item = Self>;

    fn id(&self) -> &Self::Id;
    fn title(&self) -> &str;
    fn kind(&self) -> MediaKind;
    fn monitored(&self) -> bool;
    fn quality_profile(&self) -> ProfileId;
    fn root_folder(&self) -> &RootFolder;
    fn external_ids(&self) -> &ExternalIds; // tmdb, tvdb, imdb, musicbrainz, ...

    /// Movies yield [editions]. Series yield episodes. Albums yield tracks.
    fn acquirables(&self) -> Box<dyn Iterator<Item = Self::Acquirable> + '_>;
}

/// The atomic unit a Release can satisfy.
pub trait Acquirable: Send + Sync {
    type Item: LibraryItem;
    type Id: ItemId;

    fn id(&self) -> &Self::Id;
    fn parent(&self) -> &<Self::Item as LibraryItem>::Id;
    fn status(&self) -> &AcquisitionStatus;
    fn wanted(&self) -> bool;
}

pub enum AcquisitionStatus {
    Missing,
    Searching   { since: DateTime<Utc>, attempts: u32 },
    Snatched    { release: ReleaseId, downloader: DownloaderId, at: DateTime<Utc> },
    Downloading { release: ReleaseId, progress: f32 },
    Imported    { file: FileRef, quality: Quality, score: i32, at: DateTime<Utc> },
    Cutoff,                          // imported and meets profile cutoff; won't upgrade
    Failed { reason: FailureReason, retry_at: Option<DateTime<Utc>> },
}

pub enum MediaKind {
    Movie,
    Series,
    Music,
    Book,
    Subtitle,
}
```

### Identifiers

Newtype every ID. No raw `i64` / `Uuid` floating through function signatures.

```rust
// skadi-core/src/id.rs
pub trait ItemId: Copy + Eq + Hash + Debug + Send + Sync + 'static {}

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
        pub struct $name(pub Uuid);
        impl ItemId for $name {}
    };
}

id_type!(MovieId);
id_type!(MovieEditionId);
id_type!(SeriesId);
id_type!(EpisodeId);
id_type!(ReleaseId);
id_type!(IndexerId);
id_type!(DownloaderId);
id_type!(ProfileId);
id_type!(RootFolderId);
```

UUIDs over auto-incrementing integers. Cleaner cross-instance import/export,
no merge collisions when seeding from a snapshot, no information leakage in
URLs.

### External IDs

A first-class type, not a HashMap-of-strings. Domain modules extend it with
their own provider IDs but the storage representation is uniform.

```rust
pub struct ExternalIds {
    pub tmdb:        Option<TmdbId>,
    pub tvdb:        Option<TvdbId>,
    pub imdb:        Option<ImdbId>,
    pub musicbrainz: Option<MusicBrainzId>,
    pub goodreads:   Option<GoodreadsId>,
    // serialized as JSON; nullable per-provider
}
```

## Plumbing Traits

Each is implemented in its own crate, registered with the daemon at startup.

```rust
// skadi-metadata
#[async_trait]
pub trait MetadataProvider: Send + Sync {
    fn name(&self) -> &str;
    fn supports(&self, kind: MediaKind) -> bool;
    async fn search(&self, q: &MetadataQuery) -> Result<Vec<MetadataMatch>>;
    async fn lookup(&self, id: &ExternalId) -> Result<MetadataRecord>;
    async fn refresh(&self, id: &ExternalId) -> Result<MetadataRecord>;
}

// skadi-indexers
#[async_trait]
pub trait Indexer: Send + Sync {
    fn id(&self) -> IndexerId;
    fn protocol(&self) -> Protocol;          // Torrent | Usenet
    fn supports(&self, kind: MediaKind) -> bool;
    async fn search(&self, q: &IndexerQuery) -> Result<Vec<Release>>;
    async fn capabilities(&self) -> Result<IndexerCaps>;
}

pub struct Release {
    pub indexer:    IndexerId,
    pub title:      String,
    pub fetch:      ReleaseFetch,            // .torrent url, magnet, .nzb url
    pub size:       u64,
    pub published:  DateTime<Utc>,
    pub seeders:    Option<u32>,             // None for usenet
    pub parsed:     ParsedRelease,           // structured parse of the title
}

// skadi-downloaders
#[async_trait]
pub trait Downloader: Send + Sync {
    fn id(&self) -> DownloaderId;
    fn protocol(&self) -> Protocol;
    async fn add(&self, release: &Release, category: &Category) -> Result<DownloadHandle>;
    async fn status(&self, h: &DownloadHandle) -> Result<DownloadStatus>;
    async fn remove(&self, h: &DownloadHandle, delete_data: bool) -> Result<()>;
}

// skadi-importer
#[async_trait]
pub trait Importer: Send + Sync {
    /// Take a completed download and finalize it: parse, match to a wanted
    /// acquirable, rename, and hardlink/move into the appropriate root folder.
    async fn import(&self, completed: CompletedDownload) -> Result<ImportOutcome>;
}

// skadi-notify
#[async_trait]
pub trait Notifier: Send + Sync {
    fn id(&self) -> NotifierId;
    fn channels(&self) -> &[NotificationKind];
    async fn notify(&self, event: &NotificationEvent) -> Result<()>;
}
```

### Quality

```rust
// skadi-quality
pub struct QualityProfile {
    pub id:                ProfileId,
    pub name:              String,
    pub allowed:           Vec<QualityId>,            // ordered low → high
    pub cutoff:            QualityId,                 // stop upgrading at or above this
    pub upgrade_allowed:   bool,
    pub formats:           Vec<CustomFormatScore>,    // tag-based scoring rules
    pub min_format_score:  i32,
}

pub struct Quality {
    pub id:         QualityId,
    pub resolution: Resolution,           // SD, 720p, 1080p, 2160p
    pub source:     Source,               // Bluray, WebDL, WebRip, HDTV, ...
    pub codec:      Option<Codec>,        // x264, x265, av1
    pub modifier:   Option<Modifier>,     // Remux, Proper, Repack
}

pub struct CustomFormat {
    pub id:    CustomFormatId,
    pub name:  String,
    pub rules: Vec<FormatRule>,           // regex on title, size bounds, indexer flags, ...
}
```

Quality definitions, the resolution / source taxonomy, and the default custom
formats are ported from the \*arr canon (see "Porting Strategy" below). The
release-title parser lives in `skadi-quality` because the parsed result is
what drives custom format matching.

## Storage

Diesel + `diesel_async`. Dual-backend via separate migration directories.

```rust
// skadi-store/src/lib.rs

/// Backend-agnostic store. Repository traits dispatch on this enum.
#[derive(Clone)]
pub enum Store {
    Sqlite(SqlitePool),
    Postgres(PostgresPool),
}

impl Store {
    pub async fn from_env() -> Result<Self> { /* parse SKADI_DATABASE_URL */ }
    pub async fn run_migrations(&self) -> Result<()>;
}

/// Per-domain repository traits. Implementations live in their respective
/// domain crates (e.g. skadi-movies::MoviesStoreImpl) and dispatch on the
/// Store variant for backend-specific queries.
#[async_trait]
pub trait MovieRepo: Send + Sync {
    async fn get(&self, id: MovieId) -> Result<Option<Movie>>;
    async fn list(&self, filter: &MovieFilter) -> Result<Vec<Movie>>;
    async fn upsert(&mut self, movie: &Movie) -> Result<MovieId>;
    async fn delete(&mut self, id: MovieId) -> Result<()>;
}
```

### Migrations

Two migration directories: `migrations/sqlite/` and `migrations/postgres/`,
each with the same set of timestamp-named subdirectories containing `up.sql` /
`down.sql`. Diesel supports this natively via per-backend migration paths.

Migrations are **embedded** in the binary (`diesel_migrations::embed_migrations!`)
and **always run at startup**, regardless of which domains are enabled. The
schemas for all domains exist in the DB at all times; disabled domains simply
don't write to them.

This trades a few KB of unused schema for a simpler operational model:
enable/disable is a row update, never a schema migration.

### Diesel models vs domain types

Diesel models live in `skadi-store` and are mechanical 1:1 with table rows.
Domain crates define their own richer types (`Movie`, `Series`, etc.) and
convert at the repository boundary. This keeps `skadi-core` free of Diesel
dependencies and keeps the storage layer free of business logic.

## Domain Module Registration

```rust
// skadi-core/src/module.rs
pub trait DomainModule: Send + Sync {
    fn name(&self) -> &'static str;       // "movies", "tv", ...
    fn kind(&self) -> MediaKind;

    fn migrations(&self) -> &'static [Migration];
    fn router(&self) -> axum::Router<DaemonContext>;
    fn cli(&self) -> clap::Command;
    fn handle_cli<'a>(
        &'a self,
        m: &'a clap::ArgMatches,
        ctx: &'a CliContext,
    ) -> BoxFuture<'a, Result<()>>;

    /// Background workers (hunters, scanners, refresh loops). Spawned only
    /// when the domain is enabled; cancelled when disabled.
    fn workers(&self, ctx: DaemonContext) -> Vec<BoxedWorker>;
}
```

The binary registers every domain unconditionally:

```rust
// skadi-cli/src/main.rs
fn main() -> Result<()> {
    let mut registry = DomainRegistry::new();
    registry.register(skadi_movies::module());
    registry.register(skadi_tv::module());
    registry.register(skadi_music::module());
    registry.register(skadi_books::module());

    let store = Store::from_env().await?;
    store.run_migrations().await?;

    Daemon::new(registry, store).run().await
}
```

### Runtime enable/disable

A `domains` table in the DB holds one row per registered domain:

```
domain_name | enabled | enabled_at | settings_json
```

On startup, the daemon reads this table and spawns workers only for enabled
domains. The HTTP router mounts all domain subrouters unconditionally; a
middleware checks the enabled flag and returns `412 Precondition Failed` with
a `{"error": "domain_disabled", "domain": "movies"}` body when a request hits
a disabled domain.

The supervisor watches the `domains` table (via a small pub/sub channel
inside the daemon — not via DB polling) and reacts to toggles:

- **Enable**: spawn the domain's workers, mark routes as live.
- **Disable**: signal workers to cancel via `CancellationToken`, mark routes
  as gated. Data is preserved.

Disabling is non-destructive. Re-enabling picks up exactly where it left off.

### First-run UX

```
$ skadi run
[INFO]  Skadi v0.1.0 starting
[INFO]  Database: sqlite://./skadi.db (12 migrations applied)
[INFO]  Domains registered: movies, tv, music, books
[INFO]  Domains enabled: none (configure at http://localhost:7878/setup)
[INFO]  Listening on http://0.0.0.0:7878
```

The user opens the URL and lands on a setup page that walks them through
enabling domains, adding a root folder, configuring at least one indexer, and
configuring at least one downloader. Nothing they do touches a file on disk.

## Configuration & Bootstrap

Three env vars, all with sensible defaults:

| Env var               | Default                  | Purpose                                   |
| --------------------- | ------------------------ | ----------------------------------------- |
| `SKADI_DATABASE_URL`  | `sqlite://./skadi.db`    | Diesel connection string                  |
| `SKADI_BIND_ADDR`     | `0.0.0.0:7878`           | HTTP bind                                 |
| `SKADI_SECRET_KEY`    | unset                    | If set, encrypts credential rows at rest  |

That's the entire bootstrap surface. No `config.toml`, no `[domains]` section,
no required setup file.

### Credential storage

Sensitive rows (indexer API keys, downloader passwords, TMDB tokens, webhook
secrets) live in dedicated `*_credentials` tables. When `SKADI_SECRET_KEY` is
set, those columns are encrypted at rest with AEAD (chacha20-poly1305 via
`ring` or `age`). When it isn't set, the daemon logs a `WARN` on startup but
runs anyway — first-run users don't need to generate a key to try Skadi.

Production deployments set the key. Rotation is supported via
`skadi credentials rotate` which re-encrypts under a new key in a single
transaction.

### State export/import

```bash
# Snapshot operational state — indexers, downloaders, profiles, root folders,
# custom formats, notification channels, domain enable flags.
$ skadi state export > snapshot.yaml

# Seed a fresh database from a snapshot.
$ skadi state import snapshot.yaml
```

`state export` is opinionated about scope: config rows, not library contents.
Library export is separate tooling under `skadi <domain> export`. The YAML is
human-readable and round-trippable, but editing-and-reimporting is an escape
hatch, not the supported flow for ongoing changes.

## API

`axum`, all routes under `/api/v1/`.

- `/api/v1/system/*` — health, version, enabled domains
- `/api/v1/config/*` — root folders, quality profiles, custom formats, indexers,
  downloaders, notifications, domain enable/disable
- `/api/v1/movies/*` — mounted from `skadi-movies`
- `/api/v1/tv/*`, `/api/v1/music/*`, ... — mounted from their respective modules

Versioning: `/api/v1/` is the stable surface. Breaking changes go to
`/api/v2/`. Both can coexist during a transition.

Auth: scaffolded as middleware from day one but the default is unauthenticated
loopback-only. Token auth (single bearer token in env / DB) is the first real
mode; multi-user is deferred.

CORS: off by default (same-origin only). A `skadi-ui` running on a separate
host configures allowed origins via a config row.

## UI

Dioxus web SPA in `skadi-ui`. Builds to static assets via `dx bundle --release`.
At release time, the assets are embedded into `skadi-cli` via `rust-embed` and
served at `/`. The same static assets ship as a separate tarball for users who
want to run the UI on its own host pointed at a remote daemon.

Both deployment modes consume the same API. The bundled UI has no special
in-process access.

```
┌──────────────────────────────────────────────┐
│ skadi-cli (the binary)                       │
│                                              │
│  ┌─────────────┐    ┌──────────────────────┐ │
│  │ skadi-api   │◄───┤ skadi-ui (embedded)  │ │
│  │ (axum)      │    └──────────────────────┘ │
│  └─────▲───────┘                             │
└────────┼─────────────────────────────────────┘
         │
         │  HTTP                       skadi-ui (standalone)
         │  /api/v1                       │
         │                                │
         └────────────────────────────────┘
```

A shared `skadi-client` crate exposes typed request/response models and a
`reqwest`-based client used by both the CLI and the web UI's data layer.

## Day-One: Movies

The `skadi-movies` crate.

### Domain types

```rust
// skadi-movies/src/lib.rs

pub struct Movie {
    pub id:               MovieId,
    pub title:            String,
    pub original_title:   Option<String>,
    pub year:             Option<u16>,
    pub external_ids:     ExternalIds,        // primarily TMDB for movies
    pub monitored:        bool,
    pub quality_profile:  ProfileId,
    pub root_folder:      RootFolderId,
    pub overview:         Option<String>,
    pub runtime_minutes:  Option<u32>,
    pub release_date:     Option<NaiveDate>,
    pub added_at:         DateTime<Utc>,
}

pub struct MovieEdition {
    pub id:           MovieEditionId,
    pub movie_id:     MovieId,
    pub kind:         EditionKind,            // Theatrical | Directors | Extended | ...
    pub label:        Option<String>,         // free-form e.g. "Final Cut"
    pub monitored:    bool,
    pub status:       AcquisitionStatus,
}

pub enum EditionKind {
    Theatrical,
    Directors,
    Extended,
    Unrated,
    Remastered,
    Workprint,
    Imax,
    Other,
}

impl LibraryItem for Movie {
    type Id = MovieId;
    type Acquirable = MovieEdition;
    // ...
}

impl Acquirable for MovieEdition {
    type Item = Movie;
    type Id = MovieEditionId;
    // ...
}
```

When a user adds a movie, the default is one monitored `Theatrical` edition.
Adding other editions is explicit — the UI shows known editions from TMDB and
the user picks which to monitor.

For the ~95% of movies with only one notable edition, the UX is identical to
Radarr (one row, one status). For Blade Runner / LOTR / Apocalypse Now /
Avatar / Aliens, the user can actually own the multiple cuts they want
without fighting the tool.

### Day-one impls

- **Metadata**: TMDB only.
- **Indexers**: Torznab + Newznab (covers Jackett/Prowlarr-compatible indexers
  and the vast majority of usenet).
- **Downloaders**: one torrent client + SABnzbd (one torrent, one usenet).
  *As built: a single built-in torrent client that runs inside
  the VPN namespace, so no external client is configured at all. Usenet is
  still open.*
- **Notify**: webhook only. Discord/email/Pushover are later impls of the
  same trait.
- **API**: HTTP only.
- **UI**: minimal — movie library view, add-movie search, edition picker,
  per-edition status, indexer/downloader/profile config pages, setup wizard.
- **Store**: both backends from day one with a shared test suite that runs
  against each in CI.

### Out of scope for v0

- Lists / discovery (Trakt, IMDb lists, TMDB lists)
- Calendar view
- Multi-user
- Custom format scoring beyond a baseline ported set
- Bazarr-equivalent subtitle handling
- Authentication beyond a single bearer token
- Mobile / native UIs

## Porting Strategy

A lot of what \*arr does well — release-title parsing, quality taxonomy,
custom format scoring, indexer flag handling — encodes years of community
tuning. We don't redo that thinking; we re-derive it.

Skadi is Apache-2.0 OR MIT; Sonarr v3+ and Radarr are GPL-3.0, so literal
ports are off the table. The methodology is clean-room: read the \*arr
source to understand the *pattern* and the *reasoning*, then re-derive the
implementation independently in Rust with our own naming and our own tests.
The wisdom transfers; the code does not.

**Test corpus first.** A regression suite of 5–10k real-world release titles
with expected parses is the load-bearing artifact for the parser work. It's
built before the parser, sourced from public release lists and scene group
archives — not from \*arr's test suites. Every parser revision passes the
corpus; if behavior changes, we change the corpus deliberately. This is the
single most important early deliverable for the quality layer.

## Deferred Decisions

1. **Authentication model.** Single bearer token for v0; multi-user / RBAC
   later.
2. **Subtitle handling.** Subtitles are parasitic acquirables — they attach
   to a Movie or Episode that already exists. The `Acquirable` trait
   probably handles this as-is but it's worth a thinking-pass before TV ships.
3. **Lists / discovery imports.** Trakt and IMDb-list integration is its own
   subsystem and probably wants its own trait abstraction. Defer.
4. **Release parsing crate sourcing.** Standalone Rust crate, or grown
   inside `skadi-quality`? Probably the latter for v0, extracted later if
   it grows.
5. **Reconciliation / library scan.** When a user's filesystem and Skadi's
   DB drift (manual edits, restore from backup, etc.), the scanner walks
   the root folder and matches files to library items. Design TBD; not
   blocking v0.

## Glossary

- **LibraryItem** — what a user actively tracks (movie, series, artist, ...).
- **Acquirable** — what a release satisfies (movie edition, episode, ...).
- **Release** — a candidate file located via an indexer.
- **Quality** — the structured representation of resolution + source + codec
  + modifier.
- **QualityProfile** — user's accept/reject/cutoff rules over qualities.
- **CustomFormat** — tag-based scoring rules over release titles and metadata.
- **RootFolder** — a filesystem path that holds a media library.
- **DomainModule** — a media-type plugin compiled into the binary
  (`skadi-movies`, `skadi-tv`, ...).
- **Supervisor** — the daemon component that starts/stops domain workers
  in response to enable-state changes.
