//! Adopting type-store entries into NRS without pasting URLs: searching
//! what both stores already hold (`stores_search`), binding one result
//! (`entry_adopt`), and the relations a store suggests around an entry,
//! with the entries at their other ends (`entry_related`,
//! `entry_related_add`).
//!
//! Entries bind by URL (store `"url"`), so an entry is "already in NRS"
//! when any URL a store knows it by is bound.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use nrs_core::{EntryId, RelKind};
use nrs_entry_store::{
    Direction, Directions, EntryMeta, EntryStore as _, EntrySummary, HttpEntryStore,
};
use serde::{Deserialize, Serialize};

use super::{api_error, store_error, ApiError, ApiResult, MutationResponse};
use crate::{build_relation, dah_params_json, kind_from_store, AppState, TrackedStore};

type ApiErr = (StatusCode, Json<ApiError>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StoreName {
    Anime,
    Music,
}

impl StoreName {
    fn name(self) -> &'static str {
        match self {
            Self::Anime => "anime",
            Self::Music => "music",
        }
    }

    fn client(self, state: &AppState) -> HttpEntryStore {
        match self {
            Self::Anime => state.anime_store.clone(),
            Self::Music => state.music_store.clone(),
        }
    }

    /// The store an entry URL belongs to: anime-store URLs to the anime
    /// store, every other URL to musiclib (as `entry_ingest` routes them).
    fn of_url(url: &str) -> Self {
        match nrs_import::canonical_anime_url(url) {
            Some(_) => Self::Anime,
            None => Self::Music,
        }
    }
}

/// `url` in the shape NRS binds it: anime-store URLs canonical, others as
/// the store gives them.
fn bind_form(url: &str) -> String {
    nrs_import::canonical_anime_url(url).unwrap_or_else(|| url.to_string())
}

fn locked_store(state: &AppState) -> Result<std::sync::MutexGuard<'_, TrackedStore>, ApiErr> {
    state
        .store
        .as_ref()
        .ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "adopting store entries requires database mode",
            )
        })?
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))
}

/// The NRS entry bound to any of `urls`.
fn bound_entry<'a>(
    store: &TrackedStore,
    urls: impl IntoIterator<Item = &'a String>,
) -> Result<Option<EntryId>, ApiErr> {
    for url in urls {
        if let Some(id) = store
            .find_entry("url", &bind_form(url))
            .map_err(|error| store_error("look up bound entry", error))?
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// The entry bound to `bind_url` or any of `other_urls`, else a new one
/// bound to `bind_url`, titled `title` and kinded from the store's `kind`.
/// The `bool` is whether the entry is new.
pub(super) fn bind_entry(
    store: &mut TrackedStore,
    bind_url: &str,
    other_urls: &[String],
    title: &str,
    store_kind: &str,
) -> Result<(EntryId, bool), ApiErr> {
    let bind_url = bind_form(bind_url);
    if let Some(id) = bound_entry(store, std::iter::once(&bind_url).chain(other_urls))? {
        return Ok((id, false));
    }
    let id = store
        .add_entry("url", &bind_url, Some(title))
        .map_err(|error| store_error("add entry", error))?;
    if let Some(kind) = kind_from_store(store_kind) {
        store
            .set_entry_kind(&id, Some(kind))
            .map_err(|error| store_error("set entry kind", error))?;
    }
    Ok((id, true))
}

/// `bind_entry` for a store's summary of an entry, bound by its canonical
/// URL.
fn bind_summary(
    store: &mut TrackedStore,
    summary: &EntrySummary,
) -> Result<(EntryId, bool), ApiErr> {
    let bind_url = summary
        .canonical_url
        .as_ref()
        .or(summary.urls.first())
        .ok_or_else(|| {
            api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("the store gives no URL to bind {}", summary.title),
            )
        })?;
    bind_entry(
        store,
        bind_url,
        &summary.urls,
        &summary.title,
        &summary.kind,
    )
}

fn summary_of(meta: EntryMeta) -> EntrySummary {
    EntrySummary {
        store_id: meta.store_id,
        title: meta.title,
        kind: meta.kind,
        subtitle: meta.subtitle,
        cover_url: meta.cover_url,
        release_date: meta.release_date,
        canonical_url: meta.canonical_url,
        urls: meta.urls,
    }
}

/// Runs a blocking store call off the async runtime.
async fn blocking<T: Send + 'static>(
    action: &'static str,
    call: impl FnOnce() -> nrs_entry_store::Result<T> + Send + 'static,
) -> Result<T, ApiErr> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|error| store_error(action, error))?
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("{action}: {error}")))
}

#[derive(Deserialize)]
pub(crate) struct StoreSearchQuery {
    #[serde(default)]
    q: String,
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(crate) struct StoreSearchResponse {
    results: Vec<StoreHit>,
    /// Stores that couldn't be searched, with why.
    errors: Vec<String>,
}

#[derive(Serialize)]
struct StoreHit {
    store: StoreName,
    #[serde(flatten)]
    summary: EntrySummary,
    /// The NRS entry already bound to it.
    entry_id: Option<String>,
}

/// Both stores' `search`, each best first, anime first.
pub(crate) async fn stores_search(
    State(state): State<Arc<AppState>>,
    Query(query): Query<StoreSearchQuery>,
) -> ApiResult<StoreSearchResponse> {
    drop(locked_store(&state)?);
    let q = query.q.trim().to_string();
    if q.is_empty() {
        return Ok(Json(StoreSearchResponse {
            results: Vec::new(),
            errors: Vec::new(),
        }));
    }
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    let search = |name: StoreName| {
        let client = name.client(&state);
        let q = q.clone();
        tokio::task::spawn_blocking(move || (name, client.search(&q, None, limit)))
    };
    let (anime, music) = tokio::join!(search(StoreName::Anime), search(StoreName::Music));

    let mut found = Vec::new();
    let mut errors = Vec::new();
    for joined in [anime, music] {
        let (name, result) = joined.map_err(|error| store_error("search stores", error))?;
        match result {
            Ok(summaries) => found.extend(summaries.into_iter().map(|summary| (name, summary))),
            Err(error) => errors.push(format!("{} store: {error}", name.name())),
        }
    }
    let store = locked_store(&state)?;
    let results = found
        .into_iter()
        .map(|(store_name, summary)| {
            let entry_id = bound_entry(&store, summary.canonical_url.iter().chain(&summary.urls))?;
            Ok(StoreHit {
                store: store_name,
                summary,
                entry_id: entry_id.map(|id| id.0),
            })
        })
        .collect::<Result<_, ApiErr>>()?;
    Ok(Json(StoreSearchResponse { results, errors }))
}

#[derive(Deserialize)]
pub(crate) struct AdoptRequest {
    store: StoreName,
    store_id: String,
}

/// Binds a store entry to NRS (or finds the entry already bound to it).
pub(crate) async fn entry_adopt(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AdoptRequest>,
) -> ApiResult<MutationResponse> {
    drop(locked_store(&state)?);
    let client = request.store.client(&state);
    let store_id = request.store_id;
    let meta = blocking("resolve store entry", move || client.resolve(&store_id)).await?;
    let mut store = locked_store(&state)?;
    let (id, _) = bind_summary(&mut store, &summary_of(meta))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(id.0),
    }))
}

/// One store suggestion around an entry, in NRS terms.
struct Suggestion {
    kind: RelKind,
    kind_name: String,
    direction: Direction,
    evidence: String,
    store: StoreName,
    target: EntrySummary,
}

/// The entry and what its store suggests around it: relation kinds NRS
/// writes generically (not `credit`), both directions, each with its
/// target's summary. Empty for an entry not bound by URL or a store
/// without `lookup`.
async fn suggestions_for(
    state: &Arc<AppState>,
    entry_id: &str,
) -> Result<(EntryId, Vec<Suggestion>), ApiErr> {
    let bind = {
        let store = locked_store(state)?;
        let entries = store
            .list_entries()
            .map_err(|error| store_error("load entries", error))?;
        let entry = entries
            .into_iter()
            .find(|entry| entry.id.0 == entry_id)
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "entry not found"))?;
        (entry.store == "url").then_some(entry.store_id)
    };
    let id = EntryId(entry_id.to_string());
    let Some(url) = bind else {
        return Ok((id, Vec::new()));
    };
    let store_name = StoreName::of_url(&url);
    let client = store_name.client(state);
    let suggested = blocking("load store suggestions", move || {
        let capabilities = client.capabilities()?;
        if !capabilities.operations.lookup || !capabilities.operations.suggest_relations {
            return Ok(Vec::new());
        }
        let Some(found) = client.lookup(&url)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for suggestion in client.suggest_relations(&found.store_id, Directions::Both)? {
            let target = match suggestion.target.clone() {
                Some(target) => target,
                None => summary_of(client.resolve(&suggestion.target_store_id)?),
            };
            out.push((suggestion, target));
        }
        Ok(out)
    })
    .await?;
    let suggestions = suggested
        .into_iter()
        .filter_map(|(suggestion, target)| {
            let kind = match suggestion.kind.as_str() {
                "contains" => RelKind::Contains,
                "remix" => RelKind::Remix,
                "feature_music" => RelKind::FeatureMusic,
                "killed_by" => RelKind::KilledBy,
                _ => return None,
            };
            Some(Suggestion {
                kind,
                kind_name: suggestion.kind,
                direction: suggestion.direction,
                evidence: suggestion.evidence,
                store: store_name,
                target,
            })
        })
        .collect();
    Ok((id, suggestions))
}

/// `(from, to)` of a suggestion's relation, with `target` at its other
/// end.
fn ends(entry: &EntryId, target: &EntryId, direction: Direction) -> (EntryId, EntryId) {
    match direction {
        Direction::Outgoing => (entry.clone(), target.clone()),
        Direction::Incoming => (target.clone(), entry.clone()),
    }
}

fn relation_exists(
    relations: &[nrs_store::EntryRelation],
    kind: RelKind,
    (from, to): &(EntryId, EntryId),
) -> bool {
    relations
        .iter()
        .any(|relation| relation.kind == kind && &relation.from == from && &relation.to == to)
}

#[derive(Serialize)]
pub(crate) struct RelatedResponse {
    suggestions: Vec<RelatedSuggestion>,
}

#[derive(Serialize)]
struct RelatedSuggestion {
    kind: String,
    direction: Direction,
    evidence: String,
    store: StoreName,
    target: EntrySummary,
    /// The NRS entry already bound to the target.
    target_entry_id: Option<String>,
    /// Whether NRS already records this relation.
    relation_exists: bool,
}

pub(crate) async fn entry_related(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
) -> ApiResult<RelatedResponse> {
    let (id, suggestions) = suggestions_for(&state, &entry_id).await?;
    let store = locked_store(&state)?;
    let relations = store
        .relations_for_entry(&id)
        .map_err(|error| store_error("load relations", error))?;
    let suggestions = suggestions
        .into_iter()
        .map(|suggestion| {
            let target_entry = bound_entry(
                &store,
                suggestion
                    .target
                    .canonical_url
                    .iter()
                    .chain(&suggestion.target.urls),
            )?;
            let relation_exists = target_entry.as_ref().is_some_and(|target| {
                relation_exists(
                    &relations,
                    suggestion.kind,
                    &ends(&id, target, suggestion.direction),
                )
            });
            Ok(RelatedSuggestion {
                kind: suggestion.kind_name,
                direction: suggestion.direction,
                evidence: suggestion.evidence,
                store: suggestion.store,
                target: suggestion.target,
                target_entry_id: target_entry.map(|id| id.0),
                relation_exists,
            })
        })
        .collect::<Result<_, ApiErr>>()?;
    Ok(Json(RelatedResponse { suggestions }))
}

#[derive(Deserialize)]
pub(crate) struct RelatedAddRequest {
    items: Vec<RelatedPick>,
}

#[derive(Deserialize)]
struct RelatedPick {
    store_id: String,
    kind: String,
    direction: Direction,
}

#[derive(Serialize)]
pub(crate) struct RelatedAddResponse {
    ok: bool,
    /// Entries this added to NRS.
    entries_added: usize,
    relations_added: usize,
}

/// Records picked suggestions: binds each target not yet in NRS, then adds
/// the relation unless NRS already has it. A pick must be one of the
/// entry's current suggestions.
pub(crate) async fn entry_related_add(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
    Json(request): Json<RelatedAddRequest>,
) -> ApiResult<RelatedAddResponse> {
    let (id, suggestions) = suggestions_for(&state, &entry_id).await?;
    let picked = request
        .items
        .iter()
        .map(|pick| {
            suggestions
                .iter()
                .find(|s| {
                    s.target.store_id == pick.store_id
                        && s.kind_name == pick.kind
                        && s.direction == pick.direction
                })
                .ok_or_else(|| {
                    api_error(
                        StatusCode::BAD_REQUEST,
                        format!("{} is not a current suggestion", pick.store_id),
                    )
                })
        })
        .collect::<Result<Vec<_>, ApiErr>>()?;

    let mut store = locked_store(&state)?;
    let mut entries_added = 0;
    let mut relations_added = 0;
    for suggestion in picked {
        let (target, new) = bind_summary(&mut store, &suggestion.target)?;
        entries_added += usize::from(new);
        let relations = store
            .relations_for_entry(&id)
            .map_err(|error| store_error("load relations", error))?;
        let (from, to) = ends(&id, &target, suggestion.direction);
        if relation_exists(&relations, suggestion.kind, &(from.clone(), to.clone())) {
            continue;
        }
        let fields = HashMap::from([
            ("kind".to_string(), suggestion.kind_name.clone()),
            ("from_entry".to_string(), from.0.clone()),
            ("to_entry".to_string(), to.0.clone()),
        ]);
        let relation = build_relation(&suggestion.kind_name, from, to, &fields)
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
        let relation_id = store
            .add_relation(
                relation.kind,
                &relation.from,
                &relation.to,
                &relation.transform,
            )
            .map_err(|error| store_error("add relation", error))?;
        if let Err(error) = store.set_relation_dah(&relation_id, &dah_params_json(&fields)) {
            let _ = store.delete_relation(&relation_id);
            return Err(store_error("save relation metadata", error));
        }
        relations_added += 1;
    }
    Ok(Json(RelatedAddResponse {
        ok: true,
        entries_added,
        relations_added,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use serde_json::{json, Value};

    const TRACK_URL: &str = "https://musicbrainz.org/recording/t";
    const TRACK_ALT_URL: &str = "https://youtu.be/t";
    const ALBUM_URL: &str = "https://musicbrainz.org/release/a";

    fn track() -> Value {
        json!({"store_id": "1", "title": "Track", "kind": "track", "subtitle": "Artist",
               "canonical_url": TRACK_URL, "urls": [TRACK_URL, TRACK_ALT_URL]})
    }

    fn album() -> Value {
        json!({"store_id": "2", "title": "Album", "kind": "release",
               "canonical_url": ALBUM_URL, "urls": [ALBUM_URL]})
    }

    /// A musiclib stand-in: track 1 on album 2.
    async fn mock_music_store() -> String {
        let router = axum::Router::new()
            .route(
                "/nrs-store/v1/capabilities",
                get(|| async {
                    Json(json!({"store": "musiclib", "protocol_version": "0.3",
                        "operations": {"resolve": true, "lookup": true, "search": true,
                                       "ingest": false, "suggest_relations": true},
                        "jobs": {"streaming": false}}))
                }),
            )
            .route(
                "/nrs-store/v1/search",
                get(|| async { Json(json!({"results": [track()]})) }),
            )
            .route(
                "/nrs-store/v1/lookup",
                get(|Query(q): Query<HashMap<String, String>>| async move {
                    match q["query"].as_str() {
                        TRACK_URL | TRACK_ALT_URL => Ok(Json(json!({"store_id": "1"}))),
                        ALBUM_URL => Ok(Json(json!({"store_id": "2"}))),
                        _ => Err((
                            StatusCode::NOT_FOUND,
                            Json(json!({"code": "not_found", "message": "no"})),
                        )),
                    }
                }),
            )
            .route(
                "/nrs-store/v1/entries/{id}",
                get(|Path(id): Path<String>| async move {
                    let mut meta = if id == "1" { track() } else { album() };
                    meta["extra"] = json!({});
                    Json(meta)
                }),
            )
            .route(
                "/nrs-store/v1/entries/{id}/suggested-relations",
                get(
                    |Path(id): Path<String>, Query(q): Query<HashMap<String, String>>| async move {
                        let both = q.get("directions").map(String::as_str) == Some("both");
                        let suggestions = match (id.as_str(), both) {
                            ("1", true) => vec![json!({"kind": "contains", "direction": "incoming",
                                "target_store_id": "2", "target": album(),
                                "confidence": 1.0, "evidence": "tracklist track 1"})],
                            ("2", _) => vec![json!({"kind": "contains", "direction": "outgoing",
                                "target_store_id": "1", "target": track(),
                                "confidence": 1.0, "evidence": "tracklist track 1"})],
                            _ => vec![],
                        };
                        Json(json!({"suggestions": suggestions}))
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    async fn state_with(store: nrs_store::Store) -> Arc<AppState> {
        let snapshot = store.load_snapshot().unwrap().0;
        Arc::new(AppState {
            snapshot,
            store: Some(std::sync::Mutex::new(TrackedStore::new(store))),
            // Nothing listens on the discard port.
            anime_store: HttpEntryStore::new("http://127.0.0.1:9").unwrap(),
            music_store: HttpEntryStore::new(&mock_music_store().await).unwrap(),
            music_store_url: None,
            anime_store_url: None,
            scores_cache: Default::default(),
            kind_hints: Default::default(),
        })
    }

    fn search_query(q: &str) -> Query<StoreSearchQuery> {
        Query(StoreSearchQuery {
            q: q.into(),
            limit: None,
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_marks_entries_bound_under_any_store_url() {
        let mut store = nrs_store::Store::open_in_memory().unwrap();
        let bound = store.add_entry("url", TRACK_ALT_URL, Some("Track")).unwrap();
        let state = state_with(store).await;

        let response = stores_search(State(state), search_query("track"))
            .await
            .unwrap()
            .0;
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].store, StoreName::Music);
        assert_eq!(response.results[0].entry_id.as_deref(), Some(bound.0.as_str()));
        // The unreachable anime store is reported, not fatal.
        assert_eq!(response.errors.len(), 1);
        assert!(response.errors[0].starts_with("anime store:"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_then_add_the_album_a_track_is_on() {
        let state = state_with(nrs_store::Store::open_in_memory().unwrap()).await;
        let adopt = |state: Arc<AppState>| {
            entry_adopt(
                State(state),
                Json(AdoptRequest {
                    store: StoreName::Music,
                    store_id: "1".into(),
                }),
            )
        };
        let track_id = adopt(state.clone()).await.unwrap().0.id.unwrap();
        assert_eq!(adopt(state.clone()).await.unwrap().0.id.unwrap(), track_id);
        {
            let store = state.store.as_ref().unwrap().lock().unwrap();
            assert_eq!(
                store.find_entry("url", TRACK_URL).unwrap(),
                Some(EntryId(track_id.clone()))
            );
            assert_eq!(
                store.entry_kinds().unwrap().get(&track_id).map(String::as_str),
                Some("track")
            );
        }

        let related = entry_related(State(state.clone()), Path(track_id.clone()))
            .await
            .unwrap()
            .0;
        assert_eq!(related.suggestions.len(), 1);
        let suggestion = &related.suggestions[0];
        assert_eq!(suggestion.direction, Direction::Incoming);
        assert_eq!(suggestion.target.title, "Album");
        assert_eq!(suggestion.target_entry_id, None);
        assert!(!suggestion.relation_exists);

        let add = |state: Arc<AppState>| {
            entry_related_add(
                State(state),
                Path(track_id.clone()),
                Json(RelatedAddRequest {
                    items: vec![RelatedPick {
                        store_id: "2".into(),
                        kind: "contains".into(),
                        direction: Direction::Incoming,
                    }],
                }),
            )
        };
        let added = add(state.clone()).await.unwrap().0;
        assert_eq!((added.entries_added, added.relations_added), (1, 1));
        let again = add(state.clone()).await.unwrap().0;
        assert_eq!((again.entries_added, again.relations_added), (0, 0));

        let related = entry_related(State(state.clone()), Path(track_id.clone()))
            .await
            .unwrap()
            .0;
        let album_id = related.suggestions[0].target_entry_id.clone().unwrap();
        assert!(related.suggestions[0].relation_exists);
        let store = state.store.as_ref().unwrap().lock().unwrap();
        let relations = store.relations_for_entry(&EntryId(track_id)).unwrap();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].kind, RelKind::Contains);
        assert_eq!(relations[0].from, EntryId(album_id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn related_add_rejects_a_pick_the_store_does_not_suggest() {
        let mut store = nrs_store::Store::open_in_memory().unwrap();
        let track = store.add_entry("url", TRACK_URL, Some("Track")).unwrap();
        let state = state_with(store).await;
        let result = entry_related_add(
            State(state),
            Path(track.0),
            Json(RelatedAddRequest {
                items: vec![RelatedPick {
                    store_id: "2".into(),
                    kind: "contains".into(),
                    direction: Direction::Outgoing,
                }],
            }),
        )
        .await;
        assert_eq!(result.err().unwrap().0, StatusCode::BAD_REQUEST);
    }
}
