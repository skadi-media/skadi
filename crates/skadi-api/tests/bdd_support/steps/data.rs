//! Seed data for the read-side routes: history, traces, decisions, and the
//! fake domain library that stands in for movies/tv/audiobooks.
use chrono::Utc;
use cucumber::given;

use skadi_api::{LibraryEditionDto, LibraryItemDto};
use skadi_store::{
    DecisionEntry, DecisionHistoryRepo, HistoryEntry, HistoryRepo, TraceEvent, TraceRepo,
};

use crate::bdd_support::{FakeLibrary, World, kind_for};

fn history_row(i: usize, event: &str, reason_code: Option<&str>) -> HistoryEntry {
    HistoryEntry {
        id: format!("h-{i:04}"),
        at: Utc::now() - chrono::Duration::seconds(i as i64),
        kind: "movie".into(),
        acquirable_ref: format!("movie:{}", i % 7),
        label: format!("Movie {i}"),
        event: event.into(),
        detail: Some(format!("detail {i}")),
        reason_code: reason_code.map(str::to_string),
    }
}

#[given(expr = "{int} history rows of which {int} are failed imports")]
async fn history_rows(w: &mut World, total: usize, failed: usize) {
    let store = w.store().await;
    for i in 0..total {
        let row = if i < failed {
            history_row(i, "failed", Some("import_failed"))
        } else {
            history_row(i, "grabbed", None)
        };
        store.record_history(&row).await.expect("record_history");
    }
}

#[given(expr = "{int} trace events for {string}")]
async fn trace_rows(w: &mut World, n: usize, acquirable: String) {
    let store = w.store().await;
    for i in 0..n {
        store
            .record_trace(&TraceEvent {
                id: format!("t-{acquirable}-{i}"),
                at: Utc::now() - chrono::Duration::seconds(i as i64),
                run_id: Some("run-1".into()),
                kind: "movie".into(),
                acquirable_ref: acquirable.clone(),
                stage: "searching".into(),
                event: "candidates_found".into(),
                message: format!("found {i}"),
                detail: None,
            })
            .await
            .expect("record_trace");
    }
}

#[given(expr = "a recorded decision for {string} choosing {string} with release key {string}")]
async fn decision_row(w: &mut World, acquirable: String, title: String, key: String) {
    w.store()
        .await
        .record_decision(&DecisionEntry {
            id: uuid::Uuid::new_v4().to_string(),
            at: Utc::now(),
            kind: "movie".into(),
            acquirable_ref: acquirable,
            title,
            quality: Some("Bluray-1080p".into()),
            decision: Some("Accept".into()),
            format_score: 0,
            explanation: r#"{"reason":"bdd"}"#.into(),
            release_key: Some(key),
        })
        .await
        .expect("record_decision");
}

/// `Given the "movies" library holds:` followed by a table of
/// `| title | monitored | status |` rows (status = the single edition's kind).
#[given(expr = "the {string} library holds:")]
async fn library_holds(w: &mut World, step: &cucumber::gherkin::Step, domain: String) {
    let table = step.table().expect("a table of library items");
    let mut items = Vec::new();
    for row in table.rows.iter().skip(1) {
        let title = row[0].trim().to_string();
        let monitored = row[1].trim() == "yes";
        let status = row[2].trim().to_string();
        items.push(LibraryItemDto {
            kind: format!("{:?}", kind_for(&domain)).to_lowercase(),
            id: uuid::Uuid::new_v4().to_string(),
            title,
            year: Some(2001),
            monitored,
            editions: vec![LibraryEditionDto {
                id: uuid::Uuid::new_v4().to_string(),
                kind: "theatrical".into(),
                kind_name: Some("Theatrical".into()),
                status_kind: status,
                monitored: true,
                quality: None,
                quality_name: None,
                media_info: None,
            }],
        });
    }
    if let Some(lib) = w.libraries.iter_mut().find(|l| l.domain == domain) {
        lib.items = items;
    } else {
        w.libraries.push(FakeLibrary {
            kind: kind_for(&domain),
            domain,
            items,
            occupied: Vec::new(),
        });
    }
    w.reset_api();
}

#[given(expr = "the {string} library occupies the root folder {string}")]
async fn library_occupies(w: &mut World, domain: String, folder: String) {
    let path = w.tmp().join("library").join(folder);
    if let Some(lib) = w.libraries.iter_mut().find(|l| l.domain == domain) {
        lib.occupied.push(path);
    } else {
        w.libraries.push(FakeLibrary {
            kind: kind_for(&domain),
            domain,
            items: Vec::new(),
            occupied: vec![path],
        });
    }
    w.reset_api();
}
