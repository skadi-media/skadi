//! C01 core-kernel steps: typed ids, the canonical error type, external ids,
//! transport protocol, status serialisation, root-folder probing, media tiers
//! and the NFO reader.
use std::path::PathBuf;

use chrono::Utc;
use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use uuid::Uuid;

use skadi_core::{
    AcquisitionStatus, AppError, AsinId, ExternalIds, FailureReason, FileRef, ImdbId, Protocol,
    QualityId, TmdbId, VideoInfo,
};

use crate::bdd_support::World;

// ---- typed identifiers --------------------------------------------------------

/// Exercise one id newtype by name: freshness, `Display`, `From<Uuid>` and
/// transparent serde.
macro_rules! check_id {
    ($t:ty) => {{
        let a = <$t>::new();
        let b = <$t>::new();
        assert_ne!(a, b, "fresh ids must differ");
        assert_eq!(
            a.to_string(),
            a.as_uuid().to_string(),
            "Display is the UUID"
        );
        let raw = Uuid::new_v4();
        let from = <$t>::from(raw);
        assert_eq!(from.into_uuid(), raw);
        let json = serde_json::to_string(&from).unwrap();
        assert_eq!(json, format!("\"{raw}\""), "serialises as the bare UUID");
        let back: $t = serde_json::from_str(&json).unwrap();
        assert_eq!(back, from);
        let mut set = std::collections::HashSet::new();
        set.insert(from);
        assert!(set.contains(&from), "usable as a set key");
    }};
}

#[then(expr = "the {word} identifier is a fresh, printable, JSON-transparent UUID")]
async fn id_contract(_w: &mut World, name: String) {
    match name.as_str() {
        "MovieId" => check_id!(skadi_core::MovieId),
        "MovieEditionId" => check_id!(skadi_core::MovieEditionId),
        "SeriesId" => check_id!(skadi_core::SeriesId),
        "EpisodeId" => check_id!(skadi_core::EpisodeId),
        "SeasonId" => check_id!(skadi_core::SeasonId),
        "BookId" => check_id!(skadi_core::BookId),
        "BookFileId" => check_id!(skadi_core::BookFileId),
        "AuthorId" => check_id!(skadi_core::AuthorId),
        "ReleaseId" => check_id!(skadi_core::ReleaseId),
        "IndexerId" => check_id!(skadi_core::IndexerId),
        "DownloaderId" => check_id!(skadi_core::DownloaderId),
        "ProfileId" => check_id!(skadi_core::ProfileId),
        "QualityId" => check_id!(skadi_core::QualityId),
        "CustomFormatId" => check_id!(skadi_core::CustomFormatId),
        "NotifierId" => check_id!(skadi_core::NotifierId),
        "RootFolderId" => check_id!(skadi_core::RootFolderId),
        other => panic!("unknown id type {other}"),
    }
}

#[then("a movie id and a release id are distinct types even for the same UUID")]
async fn ids_are_distinct_types(_w: &mut World) {
    let raw = Uuid::new_v4();
    let movie = skadi_core::MovieId::from(raw);
    let release = skadi_core::ReleaseId::from(raw);
    // Same bytes, different types: only the inner UUIDs can be compared.
    assert_eq!(movie.as_uuid(), release.as_uuid());
    assert_eq!(movie.to_string(), release.to_string());
}

// ---- the canonical error --------------------------------------------------------

fn error_named(variant: &str, msg: &str) -> AppError {
    match variant {
        "NotFound" => AppError::NotFound(msg.into()),
        "Validation" => AppError::Validation(msg.into()),
        "Config" => AppError::Config(msg.into()),
        "Network" => AppError::Network(msg.into()),
        "Internal" => AppError::Internal(msg.into()),
        "InvalidTransition" => AppError::InvalidTransition {
            from: "Missing".into(),
            to: "Imported".into(),
        },
        other => panic!("unknown variant {other}"),
    }
}

#[when(expr = "an {word} error is raised with message {string}")]
async fn raise(w: &mut World, variant: String, msg: String) {
    let e = error_named(&variant, &msg);
    w.error_text = Some(e.to_string());
    w.error_variant = Some(variant);
}

#[then(expr = "it displays as {string}")]
async fn displays(w: &mut World, text: String) {
    assert_eq!(w.error_text.as_deref(), Some(text.as_str()));
}

#[when("a std I/O failure is propagated with the question-mark operator")]
async fn io_from(w: &mut World) {
    fn fails() -> skadi_core::Result<()> {
        std::fs::File::open("/definitely/not/here/skadi-bdd")?;
        Ok(())
    }
    let e = fails().unwrap_err();
    w.error_variant = Some(
        match &e {
            AppError::Io(_) => "Io",
            _ => "other",
        }
        .into(),
    );
    w.error_text = Some(e.to_string());
}

#[when("an anyhow error chain is propagated with the question-mark operator")]
async fn anyhow_from(w: &mut World) {
    fn fails() -> skadi_core::Result<()> {
        let inner = anyhow::anyhow!("tracker said no").context("snatching release");
        Err(inner)?
    }
    let e = fails().unwrap_err();
    w.error_variant = Some(
        match &e {
            AppError::Other(_) => "Other",
            _ => "other",
        }
        .into(),
    );
    w.error_text = Some(e.to_string());
}

#[then(expr = "it is the {word} variant")]
async fn variant_is(w: &mut World, variant: String) {
    assert_eq!(w.error_variant.as_deref(), Some(variant.as_str()));
}

#[then(expr = "its message starts with {string}")]
async fn message_starts(w: &mut World, prefix: String) {
    let text = w.error_text.as_deref().expect("an error");
    assert!(
        text.starts_with(&prefix),
        "{text:?} does not start with {prefix:?}"
    );
}

/// Sonarr/Radarr return validation failures as a list of
/// `{ propertyName, errorMessage }` so the UI can mark the offending field
/// (SKADI-T-0525). `AppError::ValidationField` carries that field.
#[then("the validation error names the offending field")]
async fn validation_field(w: &mut World) {
    // `error_text` is the rendered `Display`, so drop the variant's own prefix to
    // get back the message the scenario supplied.
    let text = w.error_text.clone().expect("an error");
    let msg = text
        .strip_prefix("Validation error: ")
        .unwrap_or(&text)
        .to_string();
    // The scenario's message is "api_key must be a non-empty string": the field is
    // the leading token, the reason is the rest. A real caller builds this at the
    // point of failure, where both halves are already known separately.
    let (field, reason) = msg.split_once(' ').expect("message names a field");
    let e = AppError::field(field, reason);

    assert_eq!(
        e.field_name(),
        Some("api_key"),
        "the error must name the field a form would highlight"
    );
    // The rendered message leads with the field, so it is useful even where no
    // structured consumer exists (a CLI, a log line).
    assert_eq!(e.to_string(), "api_key: must be a non-empty string");
    // And a plain `Validation` still names no field — the distinction is the point
    // (~150 call sites genuinely have no single offending input, and forcing them
    // to invent one would make the field untrustworthy where a UI wants it).
    assert_eq!(AppError::Validation(msg).field_name(), None);
}

// ---- external ids / protocol ----------------------------------------------------

#[when("a library item knows only its TMDB id 603")]
async fn only_tmdb(w: &mut World) {
    let ids = ExternalIds {
        tmdb: Some(TmdbId(603)),
        ..Default::default()
    };
    w.json = Some(serde_json::to_string(&ids).unwrap());
}

#[then("its external ids serialise as exactly:")]
async fn ids_json(w: &mut World, step: &Step) {
    let want = step
        .docstring()
        .cloned()
        .unwrap_or_default()
        .trim()
        .to_string();
    assert_eq!(w.json.as_deref(), Some(want.as_str()));
    let back: ExternalIds = serde_json::from_str(w.json.as_deref().unwrap()).unwrap();
    assert_eq!(back.tmdb, Some(TmdbId(603)));
    assert!(back.imdb.is_none() && back.tvdb.is_none() && back.asin.is_none());
}

#[then("provider ids keep their upstream shape in JSON")]
async fn provider_shapes(_w: &mut World) {
    assert_eq!(serde_json::to_string(&TmdbId(603)).unwrap(), "603");
    assert_eq!(
        serde_json::to_string(&ImdbId("tt0083658".into())).unwrap(),
        r#""tt0083658""#
    );
    assert_eq!(
        serde_json::to_string(&AsinId("B08G9PRS1K".into())).unwrap(),
        r#""B08G9PRS1K""#
    );
    let empty: ExternalIds = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, ExternalIds::default());
}

#[then(expr = "the transport protocol {word} serialises as its name and back")]
async fn protocol(_w: &mut World, name: String) {
    let json = format!("\"{name}\"");
    let p = match name.as_str() {
        "Torrent" => Protocol::Torrent,
        "Usenet" => Protocol::Usenet,
        other => panic!("unknown protocol {other}"),
    };
    assert_eq!(serde_json::to_string(&p).unwrap(), json);
    let back: Protocol = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
}

// ---- status persistence ---------------------------------------------------------

#[given("a Failed status persisted before the attempts counter existed:")]
async fn legacy_failed(w: &mut World, step: &Step) {
    w.json = step.docstring().cloned();
}

#[then(expr = "it loads as Failed with {int} attempt(s) and reason code {string}")]
async fn loads_failed(w: &mut World, attempts: u32, code: String) {
    let s: AcquisitionStatus = serde_json::from_str(w.json.as_deref().unwrap()).unwrap();
    match s {
        AcquisitionStatus::Failed {
            attempts: got,
            reason,
            retry_at,
        } => {
            assert_eq!(got, attempts);
            assert_eq!(reason.code(), code);
            assert!(retry_at.is_none());
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[when(expr = "a file is imported at {string} with quality score {int}")]
async fn imported(w: &mut World, path: String, score: i32) {
    let s = AcquisitionStatus::Imported {
        file: FileRef {
            path: PathBuf::from(&path),
        },
        quality: QualityId::new(),
        score,
        at: Utc::now(),
    };
    w.json = Some(serde_json::to_string(&s).unwrap());
}

#[then(expr = "the imported status round-trips with the relative path {string} and score {int}")]
async fn imported_round_trip(w: &mut World, path: String, score: i32) {
    let s: AcquisitionStatus = serde_json::from_str(w.json.as_deref().unwrap()).unwrap();
    match s {
        AcquisitionStatus::Imported {
            file, score: got, ..
        } => {
            assert_eq!(file.path, PathBuf::from(path));
            assert!(file.path.is_relative(), "paths are relative to the root");
            assert_eq!(got, score);
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}

#[then("a download failure reason keeps its transport detail")]
async fn failure_detail(_w: &mut World) {
    let r = FailureReason::DownloadFailed("tracker timeout".into());
    let json = serde_json::to_string(&r).unwrap();
    let back: FailureReason = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
    assert_eq!(back.code(), "download_failed");
}

// ---- root folder probing ----------------------------------------------------------

/// A unique scratch directory under the OS temp dir, removed when dropped
/// (skadi-core has no `tempfile` dev-dependency).
#[derive(Debug)]
pub struct ScratchDir(pub PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tmp(w: &mut World) -> PathBuf {
    if w.tmp.is_none() {
        let dir = skadi_core::unique_temp_path("core-bdd");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        w.tmp = Some(ScratchDir(dir));
    }
    w.tmp.as_ref().unwrap().0.clone()
}

#[when("a writable directory is probed as a root folder")]
async fn probe_dir(w: &mut World) {
    let dir = tmp(w);
    w.root_status = Some(skadi_core::probe_root_status(&dir));
    let leftover = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(leftover, 0, "the write probe cleans up after itself");
}

#[when("a regular file is probed as a root folder")]
async fn probe_file(w: &mut World) {
    let file = tmp(w).join("a-file");
    std::fs::write(&file, b"x").unwrap();
    w.root_status = Some(skadi_core::probe_root_status(&file));
}

#[when("a missing path is probed as a root folder")]
async fn probe_missing(w: &mut World) {
    let missing = tmp(w).join("nope");
    w.root_status = Some(skadi_core::probe_root_status(&missing));
}

#[then("the root is usable")]
async fn usable(w: &mut World) {
    let s = w.root_status.expect("a probe");
    assert!(s.is_usable(), "{s:?}");
    assert_eq!(s.problem(), None);
}

#[then(expr = "the root is unusable because {string}")]
async fn unusable(w: &mut World, why: String) {
    let s = w.root_status.expect("a probe");
    assert!(!s.is_usable(), "{s:?}");
    assert_eq!(s.problem(), Some(why.as_str()));
}

// ---- media info / nfo ---------------------------------------------------------------

#[then(expr = "a {int}x{int} video is tier {string}")]
async fn tier(_w: &mut World, width: u32, height: u32, want: String) {
    let v = VideoInfo {
        width,
        height,
        codec: None,
        profile: None,
        dynamic_range: None,
    };
    assert_eq!(v.resolution_tier(), want);
}

#[then(expr = "the NFO unique id of type {string} in the sidecar is {string}")]
async fn nfo(_w: &mut World, kind: String, want: String) {
    let xml = r#"<tvshow>
  <title>The Expanse</title>
  <uniqueid type="tvdb" default="true">280619</uniqueid>
  <uniqueid type='imdb'>tt3230854</uniqueid>
</tvshow>"#;
    let got = skadi_core::nfo::uniqueid(xml, &kind);
    if want == "none" {
        assert_eq!(got, None);
    } else {
        assert_eq!(got.as_deref(), Some(want.as_str()));
    }
    assert_eq!(
        skadi_core::nfo::tag(xml, "title").as_deref(),
        Some("The Expanse")
    );
    assert_eq!(skadi_core::nfo::uniqueid_u64(xml, "tvdb"), Some(280_619));
}
