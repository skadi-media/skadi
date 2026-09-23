//! C23 domain-module framework steps: the `LibraryItem` / `Acquirable` contract,
//! `AcquisitionStatus` transition rules, `MediaKind` routing, `DomainModule`
//! identity/migrations and the `Worker` cancellation contract.
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use cucumber::{given, then, when};
use diesel_migrations::{EmbeddedMigrations, embed_migrations};
use tokio_util::sync::CancellationToken;

use skadi_core::module::{BoxFuture, BoxedWorker};
use skadi_core::{
    Acquirable, AcquisitionStatus, DomainModule, ExternalIds, FailureReason, FileRef, LibraryItem,
    MediaKind, MediaType, MovieEditionId, MovieId, ProfileId, QualityId, RootFolder, TmdbId,
    Worker,
};

use crate::bdd_support::World;

// ---- LibraryItem / Acquirable doubles -------------------------------------

#[derive(Debug, Clone)]
pub struct Unit {
    id: MovieEditionId,
    parent: MovieId,
    status: AcquisitionStatus,
}

#[derive(Debug)]
pub struct Item {
    id: MovieId,
    title: String,
    monitored: bool,
    profile: ProfileId,
    root: RootFolder,
    ids: ExternalIds,
    units: Vec<Unit>,
}

impl LibraryItem for Item {
    type Id = MovieId;
    type Acquirable = Unit;
    fn id(&self) -> &MovieId {
        &self.id
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Movie
    }
    fn monitored(&self) -> bool {
        self.monitored
    }
    fn quality_profile(&self) -> ProfileId {
        self.profile
    }
    fn root_folder(&self) -> &RootFolder {
        &self.root
    }
    fn external_ids(&self) -> &ExternalIds {
        &self.ids
    }
    fn acquirables(&self) -> Box<dyn Iterator<Item = Unit> + '_> {
        Box::new(self.units.iter().cloned())
    }
}

impl Acquirable for Unit {
    type Item = Item;
    type Id = MovieEditionId;
    fn id(&self) -> &MovieEditionId {
        &self.id
    }
    fn parent(&self) -> &MovieId {
        &self.parent
    }
    fn status(&self) -> &AcquisitionStatus {
        &self.status
    }
    fn wanted(&self) -> bool {
        matches!(
            self.status,
            AcquisitionStatus::Missing | AcquisitionStatus::Failed { .. }
        )
    }
}

pub fn status_named(name: &str) -> AcquisitionStatus {
    match name {
        "Missing" => AcquisitionStatus::Missing,
        "Searching" => AcquisitionStatus::Searching {
            since: Utc::now(),
            attempts: 1,
        },
        "Snatched" => AcquisitionStatus::Snatched {
            release: skadi_core::ReleaseId::new(),
            downloader: skadi_core::DownloaderId::new(),
            at: Utc::now(),
        },
        "Downloading" => AcquisitionStatus::Downloading {
            release: skadi_core::ReleaseId::new(),
            progress: 0.5,
        },
        "Imported" => AcquisitionStatus::Imported {
            file: FileRef {
                path: PathBuf::from("/lib/x.mkv"),
            },
            quality: QualityId::new(),
            score: 0,
            at: Utc::now(),
        },
        "Cutoff" => AcquisitionStatus::Cutoff,
        "Failed" => AcquisitionStatus::Failed {
            reason: FailureReason::NoSuitableRelease,
            retry_at: None,
            attempts: 0,
        },
        other => panic!("unknown status {other}"),
    }
}

#[given(expr = "a domain item {string} with acquirables in statuses {string}")]
async fn item(w: &mut World, title: String, statuses: String) {
    let id = MovieId::new();
    let units = statuses
        .split(',')
        .map(|s| Unit {
            id: MovieEditionId::new(),
            parent: id,
            status: status_named(s.trim()),
        })
        .collect();
    w.item = Some(Item {
        id,
        title,
        monitored: true,
        profile: ProfileId::new(),
        root: RootFolder::new("/lib"),
        ids: ExternalIds {
            tmdb: Some(TmdbId(603)),
            ..Default::default()
        },
        units,
    });
}

#[then(expr = "the item exposes title {string}, kind {word} and {int} acquirable(s)")]
async fn item_exposes(w: &mut World, title: String, kind: String, n: usize) {
    let item = w.item.as_ref().expect("item");
    assert_eq!(item.title(), title);
    assert_eq!(format!("{:?}", item.kind()), kind);
    assert!(item.monitored());
    assert_eq!(item.root_folder().path, PathBuf::from("/lib"));
    assert_eq!(item.external_ids().tmdb, Some(TmdbId(603)));
    let units: Vec<Unit> = item.acquirables().collect();
    assert_eq!(units.len(), n);
    assert!(units.iter().all(|u| u.parent() == item.id()));
}

#[then(expr = "exactly {int} of its acquirables is/are wanted")]
async fn wanted_count(w: &mut World, n: usize) {
    let item = w.item.as_ref().expect("item");
    assert_eq!(item.acquirables().filter(|u| u.wanted()).count(), n);
}

// ---- AcquisitionStatus transitions ----------------------------------------

#[when(expr = "an acquirable moves from {word} to {word}")]
async fn transition(w: &mut World, from: String, to: String) {
    w.transition_ok = Some(status_named(&from).can_transition_to(&status_named(&to)));
}

#[then("the transition is legal")]
async fn legal(w: &mut World) {
    assert_eq!(w.transition_ok, Some(true));
}

#[then("the transition is rejected")]
async fn illegal(w: &mut World) {
    assert_eq!(w.transition_ok, Some(false));
}

#[then(expr = "the failure reason {word} has code {string}")]
async fn reason_code(_w: &mut World, reason: String, code: String) {
    let r = match reason.as_str() {
        "NoSuitableRelease" => FailureReason::NoSuitableRelease,
        "DownloadFailed" => FailureReason::DownloadFailed("x".into()),
        "ImportFailed" => FailureReason::ImportFailed("x".into()),
        _ => FailureReason::Other("x".into()),
    };
    assert_eq!(r.code(), code);
}

#[then(expr = "the status {word} survives a JSON round-trip")]
async fn status_serde(_w: &mut World, name: String) {
    let s = status_named(&name);
    let back: AcquisitionStatus =
        serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
}

// ---- MediaKind routing -------------------------------------------------------

#[then(expr = "media kind {word} belongs to type {word} under library subfolder {string}")]
async fn media_kind(_w: &mut World, kind: String, ty: String, sub: String) {
    let k: MediaKind = serde_json::from_str(&format!("\"{kind}\"")).expect("kind");
    let want_ty = match ty.as_str() {
        "video" => MediaType::Video,
        "audio" => MediaType::Audio,
        _ => MediaType::Print,
    };
    assert_eq!(k.media_type(), want_ty);
    assert_eq!(k.media_type().as_str(), ty);
    assert_eq!(k.library_subfolder(), sub);
    assert_eq!(
        RootFolder::for_domain("/data", k).path,
        PathBuf::from("/data").join(&sub)
    );
}

// ---- DomainModule + Worker ----------------------------------------------------

const SQLITE_TEST: EmbeddedMigrations = embed_migrations!("test_migrations/sqlite");
const PG_TEST: EmbeddedMigrations = embed_migrations!("test_migrations/postgres");

#[derive(Debug)]
pub struct FlagWorker {
    pub ran: Arc<AtomicBool>,
    pub stopped: Arc<AtomicBool>,
}

impl Worker for FlagWorker {
    fn name(&self) -> &str {
        "flag"
    }
    fn run(self: Box<Self>, cancel: CancellationToken) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.ran.store(true, Ordering::SeqCst);
            cancel.cancelled().await;
            self.stopped.store(true, Ordering::SeqCst);
        })
    }
}

#[derive(Debug)]
pub struct DummyModule {
    pub ran: Arc<AtomicBool>,
    pub stopped: Arc<AtomicBool>,
}

impl DomainModule for DummyModule {
    fn name(&self) -> &'static str {
        "dummy"
    }
    fn kind(&self) -> MediaKind {
        MediaKind::Book
    }
    fn sqlite_migrations(&self) -> EmbeddedMigrations {
        SQLITE_TEST
    }
    fn postgres_migrations(&self) -> EmbeddedMigrations {
        PG_TEST
    }
    fn workers(&self) -> Vec<BoxedWorker> {
        vec![Box::new(FlagWorker {
            ran: self.ran.clone(),
            stopped: self.stopped.clone(),
        })]
    }
}

#[given("a domain module compiled in as a plug-in")]
async fn module(w: &mut World) {
    w.module = Some(DummyModule {
        ran: Arc::new(AtomicBool::new(false)),
        stopped: Arc::new(AtomicBool::new(false)),
    });
}

#[then(expr = "the module reports name {string} and kind {word}")]
async fn module_identity(w: &mut World, name: String, kind: String) {
    let m = w.module.as_ref().expect("module");
    assert_eq!(m.name(), name);
    assert_eq!(format!("{:?}", m.kind()), kind);
}

#[then("the module ships one migration set per backend")]
async fn module_migrations(w: &mut World) {
    let m = w.module.as_ref().expect("module");
    let boxed: Arc<dyn DomainModule> = Arc::new(DummyModule {
        ran: m.ran.clone(),
        stopped: m.stopped.clone(),
    });
    // Both accessors resolve and the module is object-safe (registry shape).
    let _ = boxed.sqlite_migrations();
    let _ = boxed.postgres_migrations();
    assert_eq!(boxed.name(), "dummy");
}

#[when("the supervisor spawns the module's workers and then cancels them")]
async fn spawn_cancel(w: &mut World) {
    let m = w.module.as_ref().expect("module");
    let workers = m.workers();
    assert_eq!(workers.len(), 1);
    let cancel = CancellationToken::new();
    let handles: Vec<_> = workers
        .into_iter()
        .map(|worker| {
            assert_eq!(worker.name(), "flag");
            tokio::spawn(worker.run(cancel.clone()))
        })
        .collect();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(m.ran.load(Ordering::SeqCst), "worker started");
    assert!(!m.stopped.load(Ordering::SeqCst), "worker still running");
    cancel.cancel();
    for h in handles {
        tokio::time::timeout(std::time::Duration::from_secs(2), h)
            .await
            .expect("worker returned promptly after cancel")
            .expect("worker task did not panic");
    }
}

#[then("every worker ran and stopped promptly on cancellation")]
async fn stopped(w: &mut World) {
    let m = w.module.as_ref().expect("module");
    assert!(m.ran.load(Ordering::SeqCst));
    assert!(m.stopped.load(Ordering::SeqCst));
}

/// The C23 spec's UI gap: `DomainModule` carries a machine name and kind only —
/// no display name, version or health accessor a `GET /domains` could render.
#[then("the module exposes a display name and lifecycle health for a domains screen")]
async fn no_metadata(_w: &mut World) {
    panic!(
        "DomainModule (crates/skadi-core/src/module.rs L52–78) has only name()/kind()/migrations/workers(); \
         no display name, version or health surface exists"
    );
}
