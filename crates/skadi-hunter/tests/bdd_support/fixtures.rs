//! In-process fakes for the BDD scenarios: scripted indexers/downloaders, a
//! recording notifier, place/reject matchers, and release/profile builders.
//! No network, no runner — every fake answers from memory.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;

use skadi_core::{
    AppError, DownloaderId, IndexerId, MediaKind, NotifierId, ProfileId, Protocol,
    Result as SkadiResult,
};
use skadi_downloaders::{DownloadHandle, DownloadStatus, Downloader};
use skadi_importer::{
    AcquirableMatch, AcquirableMatcher, AcquirableRef, CompletedDownload, DefaultImporter, Importer,
};
use skadi_indexers::{
    Category, Indexer, IndexerCaps, Release, ReleaseFetch, SearchQuery, TextSearch,
};
use skadi_notify::{NotificationEvent, NotificationKind, Notifier};
use skadi_quality::{QualityDefinition, QualityProfile, default_definitions, standard_profile};

/// Build a candidate release for `kind`, parsed the way `pipeline::search`
/// leaves it (movie parse for everything, TV re-parse for series).
pub fn release(kind: MediaKind, title: &str, seeders: Option<u32>, fetch: ReleaseFetch) -> Release {
    let parsed = if kind == MediaKind::Series {
        skadi_quality::parse_tv(title)
    } else {
        skadi_quality::parse(title)
    };
    Release {
        indexer: IndexerId::new(),
        title: title.to_string(),
        fetch,
        size: 4_000_000_000,
        published: Utc::now() - chrono::Duration::days(40),
        seeders,
        categories: Vec::new(),
        parsed,
    }
}

/// A magnet whose info-hash is derived from the title, so two candidates with
/// the same title share a `release_key` and different titles never collide.
pub fn magnet_for(title: &str) -> ReleaseFetch {
    let hash: String = title
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
        .to_string();
    ReleaseFetch::Magnet(format!("magnet:?xt=urn:btih:{hash:0>40}"))
}

/// The standard out-of-the-box profile (720p+ allowed, Bluray-1080p cutoff,
/// upgrades on) over the built-in definitions.
pub fn standard() -> (QualityProfile, Vec<QualityDefinition>) {
    let defs = default_definitions();
    (standard_profile(&defs), defs)
}

/// Look a built-in quality definition up by its display name (`Bluray-1080p`).
pub fn quality_named(defs: &[QualityDefinition], name: &str) -> skadi_core::QualityId {
    defs.iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no built-in quality named {name:?}"))
        .id
}

/// A profile allowing exactly the named qualities (low → high), cutoff at the
/// last one, upgrades on.
pub fn profile_of(defs: &[QualityDefinition], names: &[&str]) -> QualityProfile {
    let allowed: Vec<_> = names.iter().map(|n| quality_named(defs, n)).collect();
    QualityProfile {
        id: ProfileId::new(),
        name: names.join(","),
        cutoff: *allowed.last().expect("at least one quality"),
        allowed,
        upgrade_allowed: true,
        formats: vec![],
        min_format_score: 0,
    }
}

// ---------------------------------------------------------------------------
// Indexer
// ---------------------------------------------------------------------------

/// An indexer that answers every search with a fixed list (or an error) and
/// serves a fixed RSS feed.
pub struct ScriptedIndexer {
    pub id: IndexerId,
    pub kind: MediaKind,
    pub releases: Vec<Release>,
    pub fails: bool,
    pub feed: Vec<Release>,
    pub searches: AtomicUsize,
}

impl ScriptedIndexer {
    pub fn serving(kind: MediaKind, releases: Vec<Release>) -> Arc<dyn Indexer> {
        Arc::new(Self {
            id: IndexerId::new(),
            kind,
            releases,
            fails: false,
            feed: Vec::new(),
            searches: AtomicUsize::new(0),
        })
    }

    pub fn failing(kind: MediaKind) -> Arc<dyn Indexer> {
        Arc::new(Self {
            id: IndexerId::new(),
            kind,
            releases: Vec::new(),
            fails: true,
            feed: Vec::new(),
            searches: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl Indexer for ScriptedIndexer {
    fn id(&self) -> IndexerId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }
    fn supports(&self, kind: MediaKind) -> bool {
        self.kind == kind
    }
    async fn capabilities(&self) -> SkadiResult<IndexerCaps> {
        Ok(IndexerCaps {
            supports_rss: true,
            supports_search: true,
            id_params: Default::default(),
            supports_aggregate_ids: false,
            text_search: TextSearch::Raw,
            categories: vec![],
        })
    }
    async fn search(&self, _query: &dyn SearchQuery) -> SkadiResult<Vec<Release>> {
        self.searches.fetch_add(1, Ordering::SeqCst);
        if self.fails {
            return Err(AppError::Network("indexer down".into()));
        }
        Ok(self.releases.clone())
    }
    async fn rss(&self) -> SkadiResult<Vec<Release>> {
        Ok(self.feed.clone())
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Downloader
// ---------------------------------------------------------------------------

/// A torrent client scripted with the statuses it reports poll by poll (the
/// last one repeats). Counts adds and removes; can refuse adds.
pub struct ScriptedDownloader {
    pub id: DownloaderId,
    pub protocol: Protocol,
    pub statuses: Mutex<VecDeque<DownloadStatus>>,
    pub adds: AtomicUsize,
    pub removes: AtomicUsize,
    pub add_fails: bool,
    /// Every release title handed to `add`, in order.
    pub added_titles: Mutex<Vec<String>>,
}

impl ScriptedDownloader {
    pub fn new(protocol: Protocol, statuses: Vec<DownloadStatus>) -> Arc<Self> {
        Arc::new(Self {
            id: DownloaderId::new(),
            protocol,
            statuses: Mutex::new(statuses.into()),
            adds: AtomicUsize::new(0),
            removes: AtomicUsize::new(0),
            add_fails: false,
            added_titles: Mutex::new(Vec::new()),
        })
    }

    pub fn refusing(protocol: Protocol) -> Arc<Self> {
        Arc::new(Self {
            id: DownloaderId::new(),
            protocol,
            statuses: Mutex::new(VecDeque::new()),
            adds: AtomicUsize::new(0),
            removes: AtomicUsize::new(0),
            add_fails: true,
            added_titles: Mutex::new(Vec::new()),
        })
    }

    pub fn adds(&self) -> usize {
        self.adds.load(Ordering::SeqCst)
    }

    pub fn removes(&self) -> usize {
        self.removes.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Downloader for ScriptedDownloader {
    fn id(&self) -> DownloaderId {
        self.id
    }
    fn protocol(&self) -> Protocol {
        self.protocol
    }
    async fn add(&self, r: &Release, c: &Category) -> SkadiResult<DownloadHandle> {
        if self.add_fails {
            return Err(AppError::Network("client refused the add".into()));
        }
        self.adds.fetch_add(1, Ordering::SeqCst);
        self.added_titles.lock().unwrap().push(r.title.clone());
        Ok(DownloadHandle {
            native_id: format!("h-{}", skadi_indexers::release_key(r)),
            category: c.0.to_string(),
        })
    }
    async fn status(&self, _h: &DownloadHandle) -> SkadiResult<DownloadStatus> {
        let mut q = self.statuses.lock().unwrap();
        if q.len() > 1 {
            Ok(q.pop_front().unwrap())
        } else {
            Ok(q.front().cloned().unwrap_or(DownloadStatus::Queued))
        }
    }
    async fn remove(&self, _h: &DownloadHandle, _delete: bool) -> SkadiResult<()> {
        self.removes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Importer
// ---------------------------------------------------------------------------

/// Places every file into one directory, crediting it to `acquirable`.
pub struct PlaceInDir {
    pub dir: PathBuf,
    pub acquirable: AcquirableRef,
}

impl AcquirableMatcher for PlaceInDir {
    fn match_file(
        &self,
        _parsed: &skadi_quality::ParsedRelease,
        source: &Path,
        _completed: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        let name = source.file_name().map(PathBuf::from).unwrap_or_default();
        vec![AcquirableMatch::new(
            self.acquirable.clone(),
            self.dir.join(name),
        )]
    }
}

/// Matches nothing — every import places zero files.
pub struct RejectAll;

impl AcquirableMatcher for RejectAll {
    fn match_file(
        &self,
        _: &skadi_quality::ParsedRelease,
        _: &Path,
        _: &CompletedDownload,
    ) -> Vec<AcquirableMatch> {
        Vec::new()
    }
}

pub fn importer_into(dir: PathBuf, acquirable: &str) -> Arc<dyn Importer> {
    Arc::new(DefaultImporter::new(PlaceInDir {
        dir,
        acquirable: AcquirableRef(acquirable.to_string()),
    }))
}

pub fn rejecting_importer() -> Arc<dyn Importer> {
    Arc::new(DefaultImporter::new(RejectAll))
}

// ---------------------------------------------------------------------------
// Notifier
// ---------------------------------------------------------------------------

/// Records every event delivered; optionally fails every delivery.
pub struct RecordingNotifier {
    pub id: NotifierId,
    pub channels: Vec<NotificationKind>,
    pub seen: Mutex<Vec<NotificationEvent>>,
    pub fails: bool,
}

impl RecordingNotifier {
    pub fn wanting(channels: Vec<NotificationKind>) -> Arc<Self> {
        Arc::new(Self {
            id: NotifierId::new(),
            channels,
            seen: Mutex::new(Vec::new()),
            fails: false,
        })
    }

    pub fn failing() -> Arc<Self> {
        Arc::new(Self {
            id: NotifierId::new(),
            channels: vec![NotificationKind::Imported, NotificationKind::Grabbed],
            seen: Mutex::new(Vec::new()),
            fails: true,
        })
    }

    pub fn kinds_seen(&self) -> Vec<NotificationKind> {
        self.seen.lock().unwrap().iter().map(|e| e.kind()).collect()
    }
}

#[async_trait]
impl Notifier for RecordingNotifier {
    fn id(&self) -> NotifierId {
        self.id
    }
    fn channels(&self) -> &[NotificationKind] {
        &self.channels
    }
    async fn notify(&self, event: &NotificationEvent) -> SkadiResult<()> {
        if self.fails {
            return Err(AppError::Network("webhook 500".into()));
        }
        self.seen.lock().unwrap().push(event.clone());
        Ok(())
    }
    async fn test(&self) -> SkadiResult<()> {
        Ok(())
    }
}
