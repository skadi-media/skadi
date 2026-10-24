//! The new-imports list (SKADI-T-0744, initiative COLLIERY-I-0653): media new to
//! the library, newest first, as each member may see it.
//!
//! The rows are the acquisition history's `imported` events, which since
//! SKADI-T-0743 mean exactly "a file new to the library" (a replayed import
//! writes none, an upload writes one, an adoption of an existing tree none).
//! History stores an opaque ref per row and knows nothing of ratings, genres
//! or posters, so each domain's [`LibraryProvider::resolve_imports`] turns its
//! refs into items and drops what the member's policy hides or what is gone.
//!
//! **Why a keyset cursor.** The policy is applied after the read, so a page of
//! N visible rows can need more than N history rows, and an offset into the
//! history would not be an offset into what the member sees. The cursor names
//! the last row a page read; the next page starts after it, so imports landing
//! while a client scrolls never repeat or skip a row.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Extension, Query, State};
use axum::response::IntoResponse;
use axum::{Json, Router, routing::get};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use skadi_core::{AppError, MediaKind};
use skadi_store::{HistoryCursor, HistoryEntry, HistoryQuery, HistoryRepo};

use crate::error::ApiError;
use crate::household::{Member, member_or_admin};
use crate::library::{ImportedItemDto, LibraryProvider, history_kind};
use crate::state::AppState;

/// Rows per page when the client names no limit.
const DEFAULT_LIMIT: usize = 20;
/// The largest page served.
const MAX_LIMIT: usize = 100;
/// History rows read per query while a page fills.
const BATCH: i64 = 100;
/// History rows one page may read before it returns what it has. A member
/// whose policy hides almost everything would otherwise walk the whole log in
/// one request; with the cap the page comes back short with a cursor, and the
/// next scroll carries on.
const PAGE_MAX_SCAN: usize = 2_000;
/// The window of the count: the badge says "new in the last week".
pub const RECENT_DAYS: i64 = 7;
/// History rows the count may read. A week of imports is far below this.
const COUNT_MAX_SCAN: usize = 10_000;

/// The query of both routes.
#[derive(Deserialize, Default)]
pub struct NewImportsQuery {
    /// `movie`, `series` or `audiobook`; all kinds when absent.
    kind: Option<String>,
    /// The `next_cursor` of the previous page.
    cursor: Option<String>,
    limit: Option<usize>,
}

/// One new import (SKADI-T-0744).
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct NewImportDto {
    /// The history row id.
    pub id: String,
    /// When the file landed in the library.
    pub at: DateTime<Utc>,
    /// `movie`, `series` or `audiobook`.
    pub kind: String,
    /// The quality name recorded at import, e.g. `Bluray-1080p`.
    pub quality: Option<String>,
    /// The file replaced one the library already held.
    pub upgrade: bool,
    #[serde(flatten)]
    pub item: ImportedItemDto,
}

/// One page of the list.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct NewImportsPage {
    pub items: Vec<NewImportDto>,
    /// Pass as `cursor` for the next page; `None` when the list is done.
    pub next_cursor: Option<String>,
}

/// How many new imports the member may see in the last [`RECENT_DAYS`] days.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct NewImportsCount {
    pub count: usize,
    /// The start of the window.
    pub since: DateTime<Utc>,
}

/// The new-imports routes, merged into the authed API router.
pub fn new_imports_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/new-imports", get(list))
        .route("/new-imports/count", get(count))
}

/// The providers that hold imports, each with the history kind it owns,
/// narrowed to `kind` when the client names one.
fn sources(
    library: &[Arc<dyn LibraryProvider>],
    kind: Option<&str>,
) -> Result<Vec<(&'static str, Arc<dyn LibraryProvider>)>, ApiError> {
    let wanted = match kind {
        None => None,
        Some(k) => Some(match k {
            "movie" => MediaKind::Movie,
            "series" => MediaKind::Series,
            "audiobook" => MediaKind::Audiobook,
            other => {
                return Err(ApiError(AppError::Validation(format!(
                    "kind must be movie, series or audiobook, got {other:?}"
                ))));
            }
        }),
    };
    Ok(library
        .iter()
        .filter(|p| wanted.is_none_or(|w| p.kind() == w))
        .filter_map(|p| history_kind(p.kind()).map(|hk| (hk, Arc::clone(p))))
        .collect())
}

/// The cursor as the client carries it: `<RFC 3339 time>~<row id>`.
fn encode_cursor(c: &HistoryCursor) -> String {
    format!(
        "{}~{}",
        c.at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
        c.id
    )
}

fn decode_cursor(s: &str) -> Result<HistoryCursor, ApiError> {
    let bad = || {
        ApiError(AppError::Validation(format!(
            "not a new-imports cursor: {s:?}"
        )))
    };
    let (at, id) = s.split_once('~').ok_or_else(bad)?;
    let at = DateTime::parse_from_rfc3339(at).map_err(|_| bad())?;
    if id.is_empty() {
        return Err(bad());
    }
    Ok(HistoryCursor {
        at: at.with_timezone(&Utc),
        id: id.to_string(),
    })
}

/// What a scan found, and where it stopped when there may be more.
struct Scan {
    items: Vec<NewImportDto>,
    next: Option<HistoryCursor>,
}

/// Read `imported` rows newest first after `after`, resolve each batch through
/// its domain, and keep the rows `member` may see — until `want` rows are kept,
/// the log ends, or `max_scan` rows have been read.
async fn scan(
    store: &dyn HistoryRepo,
    sources: &[(&'static str, Arc<dyn LibraryProvider>)],
    member: &Member,
    mut after: Option<HistoryCursor>,
    since: Option<DateTime<Utc>>,
    want: usize,
    max_scan: usize,
) -> Result<Scan, ApiError> {
    // One kind narrows the read itself; several are filtered per row.
    let query = HistoryQuery {
        event: Some("imported".into()),
        kind: match sources {
            [(hk, _)] => Some((*hk).to_string()),
            _ => None,
        },
        since,
        ..HistoryQuery::default()
    };
    let mut items = Vec::new();
    let mut scanned = 0usize;
    loop {
        let rows = store
            .list_history_after(&query, after.as_ref(), BATCH)
            .await?;
        let full = rows.len() as i64 == BATCH;
        scanned += rows.len();
        let resolved = resolve(sources, member, &rows).await;
        for row in &rows {
            let cursor = HistoryCursor {
                at: row.at,
                id: row.id.clone(),
            };
            if let Some(dto) = to_dto(row, &resolved) {
                items.push(dto);
                if items.len() == want {
                    return Ok(Scan {
                        items,
                        next: Some(cursor),
                    });
                }
            }
            after = Some(cursor);
        }
        if !full {
            return Ok(Scan { items, next: None });
        }
        if scanned >= max_scan {
            return Ok(Scan { items, next: after });
        }
    }
}

/// Each history kind's resolved refs for one batch of rows. A provider that
/// fails is logged and its rows left out, as the calendar does: one domain's
/// trouble should not blank the list.
async fn resolve(
    sources: &[(&'static str, Arc<dyn LibraryProvider>)],
    member: &Member,
    rows: &[HistoryEntry],
) -> HashMap<&'static str, HashMap<String, ImportedItemDto>> {
    let mut out = HashMap::new();
    for (hk, provider) in sources {
        let refs: Vec<String> = rows
            .iter()
            .filter(|r| r.kind == *hk)
            .map(|r| r.acquirable_ref.clone())
            .collect();
        if refs.is_empty() {
            continue;
        }
        match provider.resolve_imports(member, &refs).await {
            Ok(map) => {
                out.insert(*hk, map);
            }
            Err(e) => tracing::warn!(
                domain = provider.domain(),
                error = %e,
                "new imports: resolving refs failed; omitting them"
            ),
        }
    }
    out
}

fn to_dto(
    row: &HistoryEntry,
    resolved: &HashMap<&'static str, HashMap<String, ImportedItemDto>>,
) -> Option<NewImportDto> {
    let item = resolved.get(row.kind.as_str())?.get(&row.acquirable_ref)?;
    let kind = match row.kind.as_str() {
        "episode" => "series",
        k => k,
    };
    Some(NewImportDto {
        id: row.id.clone(),
        at: row.at,
        kind: kind.to_string(),
        quality: row.detail.clone(),
        upgrade: row.upgrade,
        item: item.clone(),
    })
}

fn store(state: &AppState) -> Result<&skadi_store::Store, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(|| ApiError(AppError::Internal("no store configured".into())))
}

/// `GET /new-imports?kind=&cursor=&limit=` — media new to the library, newest
/// first, as this member may see it.
async fn list(
    State(state): State<Arc<AppState>>,
    member: Option<Extension<Member>>,
    Query(q): Query<NewImportsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let member = member_or_admin(member);
    let sources = sources(&state.library, q.kind.as_deref())?;
    let after = q.cursor.as_deref().map(decode_cursor).transpose()?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let found = scan(
        store(&state)?,
        &sources,
        &member,
        after,
        None,
        limit,
        PAGE_MAX_SCAN,
    )
    .await?;
    Ok(Json(NewImportsPage {
        items: found.items,
        next_cursor: found.next.as_ref().map(encode_cursor),
    }))
}

/// `GET /new-imports/count?kind=` — how many of the last [`RECENT_DAYS`] days'
/// new imports this member may see. The same filter as the list.
async fn count(
    State(state): State<Arc<AppState>>,
    member: Option<Extension<Member>>,
    Query(q): Query<NewImportsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let member = member_or_admin(member);
    let sources = sources(&state.library, q.kind.as_deref())?;
    let since = Utc::now() - chrono::Duration::days(RECENT_DAYS);
    let found = scan(
        store(&state)?,
        &sources,
        &member,
        None,
        Some(since),
        usize::MAX,
        COUNT_MAX_SCAN,
    )
    .await?;
    Ok(Json(NewImportsCount {
        count: found.items.len(),
        since,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::household::Role;

    /// A domain whose resolver knows some refs, and hides some from a kid.
    struct Fake {
        kind: MediaKind,
        known: Vec<String>,
        kid_hidden: Vec<String>,
    }

    #[async_trait::async_trait]
    impl LibraryProvider for Fake {
        fn domain(&self) -> &str {
            "fake"
        }
        fn kind(&self) -> MediaKind {
            self.kind
        }
        async fn items(
            &self,
            _monitored: Option<bool>,
        ) -> skadi_core::Result<Vec<crate::library::LibraryItemDto>> {
            Ok(Vec::new())
        }
        async fn resolve_imports(
            &self,
            member: &Member,
            refs: &[String],
        ) -> skadi_core::Result<HashMap<String, ImportedItemDto>> {
            Ok(refs
                .iter()
                .filter(|r| self.known.contains(r))
                .filter(|r| member.role != Role::Kid || !self.kid_hidden.contains(r))
                .map(|r| {
                    (
                        r.clone(),
                        ImportedItemDto {
                            item_id: format!("item-{r}"),
                            title: r.clone(),
                            episode: None,
                            episode_title: None,
                            year: None,
                            poster_url: None,
                        },
                    )
                })
                .collect())
        }
    }

    async fn store() -> skadi_store::Store {
        let path = skadi_core::unique_temp_path("new-imports").with_extension("db");
        let store = skadi_store::Store::connect(&format!("sqlite://{}", path.display())).unwrap();
        store.run_migrations().await.unwrap();
        store
    }

    fn row(id: &str, kind: &str, event: &str, ago_hours: i64) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            at: Utc::now() - chrono::Duration::hours(ago_hours),
            kind: kind.into(),
            acquirable_ref: format!("ref-{id}"),
            label: id.into(),
            event: event.into(),
            detail: Some("Bluray-1080p".into()),
            reason_code: None,
            upgrade: id.starts_with('u'),
        }
    }

    fn member(role: Role) -> Member {
        Member {
            role,
            ..Member::open_mode_admin()
        }
    }

    fn fake(kind: MediaKind, known: &[&str], kid_hidden: &[&str]) -> Arc<dyn LibraryProvider> {
        Arc::new(Fake {
            kind,
            known: known.iter().map(|k| format!("ref-{k}")).collect(),
            kid_hidden: kid_hidden.iter().map(|k| format!("ref-{k}")).collect(),
        })
    }

    fn ids(v: &[NewImportDto]) -> Vec<&str> {
        v.iter().map(|d| d.id.as_str()).collect()
    }

    #[tokio::test]
    async fn pages_walk_newest_first_and_end_without_a_cursor() {
        let store = store().await;
        for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            store
                .record_history(&row(id, "movie", "imported", 10 - i as i64))
                .await
                .unwrap();
        }
        // Grabs and failures are not imports.
        store
            .record_history(&row("g", "movie", "grabbed", 0))
            .await
            .unwrap();
        let lib = [fake(MediaKind::Movie, &["a", "b", "c", "d", "e", "g"], &[])];
        let src = sources(&lib, None).unwrap();
        let admin = member(Role::Admin);

        let p1 = scan(&store, &src, &admin, None, None, 2, PAGE_MAX_SCAN)
            .await
            .unwrap();
        assert_eq!(ids(&p1.items), ["e", "d"]);
        let p2 = scan(&store, &src, &admin, p1.next, None, 2, PAGE_MAX_SCAN)
            .await
            .unwrap();
        assert_eq!(ids(&p2.items), ["c", "b"]);
        let p3 = scan(&store, &src, &admin, p2.next, None, 2, PAGE_MAX_SCAN)
            .await
            .unwrap();
        assert_eq!(ids(&p3.items), ["a"]);
        assert!(p3.next.is_none(), "the list is done");
        assert_eq!(p1.items[0].quality.as_deref(), Some("Bluray-1080p"));
        assert_eq!(p1.items[0].item.item_id, "item-ref-e");
    }

    #[tokio::test]
    async fn a_kid_page_is_full_of_what_the_kid_may_see() {
        let store = store().await;
        for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            store
                .record_history(&row(id, "movie", "imported", 10 - i as i64))
                .await
                .unwrap();
        }
        let lib = [fake(
            MediaKind::Movie,
            &["a", "b", "c", "d", "e"],
            &["e", "c"],
        )];
        let src = sources(&lib, None).unwrap();
        let kid = member(Role::Kid);
        let p = scan(&store, &src, &kid, None, None, 2, PAGE_MAX_SCAN)
            .await
            .unwrap();
        assert_eq!(
            ids(&p.items),
            ["d", "b"],
            "hidden rows skipped, page still full"
        );
        let rest = scan(&store, &src, &kid, p.next, None, 10, PAGE_MAX_SCAN)
            .await
            .unwrap();
        assert_eq!(ids(&rest.items), ["a"]);
        // The admin sees all five.
        let all = scan(
            &store,
            &src,
            &member(Role::Admin),
            None,
            None,
            10,
            PAGE_MAX_SCAN,
        )
        .await
        .unwrap();
        assert_eq!(all.items.len(), 5);
    }

    #[tokio::test]
    async fn a_ref_the_domain_cannot_resolve_is_left_out() {
        let store = store().await;
        store
            .record_history(&row("gone", "movie", "imported", 2))
            .await
            .unwrap();
        store
            .record_history(&row("here", "movie", "imported", 1))
            .await
            .unwrap();
        let lib = [fake(MediaKind::Movie, &["here"], &[])];
        let src = sources(&lib, None).unwrap();
        let p = scan(
            &store,
            &src,
            &member(Role::Admin),
            None,
            None,
            10,
            PAGE_MAX_SCAN,
        )
        .await
        .unwrap();
        assert_eq!(ids(&p.items), ["here"]);
    }

    #[tokio::test]
    async fn the_kind_filter_narrows_and_episodes_read_as_series() {
        let store = store().await;
        store
            .record_history(&row("m", "movie", "imported", 3))
            .await
            .unwrap();
        store
            .record_history(&row("u-ep", "episode", "imported", 2))
            .await
            .unwrap();
        store
            .record_history(&row("b", "audiobook", "imported", 1))
            .await
            .unwrap();
        let lib = [
            fake(MediaKind::Movie, &["m"], &[]),
            fake(MediaKind::Series, &["u-ep"], &[]),
            fake(MediaKind::Audiobook, &["b"], &[]),
        ];
        let admin = member(Role::Admin);
        let all = scan(
            &store,
            &sources(&lib, None).unwrap(),
            &admin,
            None,
            None,
            10,
            PAGE_MAX_SCAN,
        )
        .await
        .unwrap();
        assert_eq!(ids(&all.items), ["b", "u-ep", "m"]);
        let tv = scan(
            &store,
            &sources(&lib, Some("series")).unwrap(),
            &admin,
            None,
            None,
            10,
            PAGE_MAX_SCAN,
        )
        .await
        .unwrap();
        assert_eq!(ids(&tv.items), ["u-ep"]);
        assert_eq!(tv.items[0].kind, "series");
        assert!(tv.items[0].upgrade);
        assert!(sources(&lib, Some("music")).is_err());
    }

    #[tokio::test]
    async fn a_page_that_reads_its_cap_comes_back_short_with_a_cursor() {
        let store = store().await;
        for i in 0..250 {
            store
                .record_history(&row(&format!("h{i:03}"), "movie", "imported", 300 - i))
                .await
                .unwrap();
        }
        store
            .record_history(&row("seen", "movie", "imported", 400))
            .await
            .unwrap();
        // The kid may see only the oldest row.
        let hidden: Vec<String> = (0..250).map(|i| format!("h{i:03}")).collect();
        let hidden: Vec<&str> = hidden.iter().map(String::as_str).collect();
        let mut known = hidden.clone();
        known.push("seen");
        let lib = [fake(MediaKind::Movie, &known, &hidden)];
        let src = sources(&lib, None).unwrap();
        let kid = member(Role::Kid);
        let p = scan(&store, &src, &kid, None, None, 5, 100).await.unwrap();
        assert!(p.items.is_empty());
        assert!(p.next.is_some(), "more may follow");
        let mut next = p.next;
        let mut found = Vec::new();
        while let Some(c) = next {
            let p = scan(&store, &src, &kid, Some(c), None, 5, 100)
                .await
                .unwrap();
            found.extend(p.items.into_iter().map(|d| d.id));
            next = p.next;
        }
        assert_eq!(found, ["seen"]);
    }

    #[tokio::test]
    async fn the_count_covers_the_window_and_the_policy() {
        let store = store().await;
        store
            .record_history(&row("old", "movie", "imported", 24 * 8))
            .await
            .unwrap();
        store
            .record_history(&row("a", "movie", "imported", 24 * 2))
            .await
            .unwrap();
        store
            .record_history(&row("b", "movie", "imported", 1))
            .await
            .unwrap();
        let lib = [fake(MediaKind::Movie, &["old", "a", "b"], &["b"])];
        let src = sources(&lib, None).unwrap();
        let since = Some(Utc::now() - chrono::Duration::days(RECENT_DAYS));
        let count = |role| {
            let (store, src) = (&store, &src);
            async move {
                scan(
                    store,
                    src,
                    &member(role),
                    None,
                    since,
                    usize::MAX,
                    COUNT_MAX_SCAN,
                )
                .await
                .unwrap()
                .items
                .len()
            }
        };
        assert_eq!(count(Role::Admin).await, 2, "the 8-day-old row is outside");
        assert_eq!(count(Role::Kid).await, 1, "the hidden row is not counted");
    }

    #[test]
    fn a_cursor_round_trips_and_garbage_is_refused() {
        let c = HistoryCursor {
            at: DateTime::from_timestamp(1_700_000_000, 123_456_789).unwrap(),
            id: "0b5e-row".into(),
        };
        assert_eq!(decode_cursor(&encode_cursor(&c)).unwrap(), c);
        assert!(decode_cursor("nope").is_err());
        assert!(decode_cursor("2026-10-24T00:00:00Z~").is_err());
        assert!(decode_cursor("yesterday~abc").is_err());
    }
}
