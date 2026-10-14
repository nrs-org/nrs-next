//! Versioned JSON API for the dependency-free browser client.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use nrs_core::{Entry, EntryId, ImpactId, RelKind, SourceId, TimeParams, TimeSupport, Transform};
use nrs_entry_store::EntryStore as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::score_filter::{Candidate, ScoreFilter};
use super::{
    build_impact_atoms, build_relation, dah_factor_field, dah_params_json, entry_display_name,
    entry_kind, entry_kind_slug, impact_anchor_mix, impact_kind_presentation,
    impact_period_summary, impact_primary_metric, impact_section, inferred_kind, kind_from_store,
    normalize_duration_fields, normalize_period_fields, parsed_dah_params, rows, used_anchors,
    AppState, ImpactSection, KindHints, SortBy, SortDir, TrackedStore, ENTRY_KINDS,
};

#[derive(Serialize)]
pub(super) struct ApiError {
    error: String,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

fn api_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
}

/// Unknown `/api/v1` paths get a JSON 404 rather than falling through
/// to the client app's `index.html`.
pub(super) async fn not_found() -> (StatusCode, Json<ApiError>) {
    api_error(StatusCode::NOT_FOUND, "no such API route")
}

fn store_error(action: &str, error: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("failed to {action}: {error}"),
    )
}

fn scoped_impact(
    store: &nrs_store::Store,
    entry_id: &str,
    impact_id: &str,
) -> Result<nrs_store::ReviewImpact, (StatusCode, Json<ApiError>)> {
    store
        .impacts_for_entry(&EntryId(entry_id.into()))
        .map_err(|error| store_error("load impact", error))?
        .into_iter()
        .find(|impact| impact.id.0 == impact_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "impact not found on this entry"))
}

fn scoped_relation(
    store: &nrs_store::Store,
    entry_id: &str,
    relation_id: &str,
) -> Result<nrs_store::EntryRelation, (StatusCode, Json<ApiError>)> {
    store
        .relations_for_entry(&EntryId(entry_id.into()))
        .map_err(|error| store_error("load relation", error))?
        .into_iter()
        .find(|relation| relation.id == relation_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "relation not found on this entry"))
}

#[derive(Serialize)]
pub(super) struct MutationResponse {
    ok: bool,
    id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct IngestRequest {
    query: String,
}

pub(super) async fn entry_ingest(
    State(state): State<Arc<AppState>>,
    Json(request): Json<IngestRequest>,
) -> ApiResult<MutationResponse> {
    if state.store.is_none() {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "entry ingest requires database mode",
        ));
    }
    let query = request.query.trim().to_string();
    if !(query.starts_with("https://") || query.starts_with("http://")) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "paste the entry's http(s) URL",
        ));
    }
    // Every ingested entry binds by URL (store "url"), like a `<bind url>`.
    // Anime-store URLs go there, every other URL to musiclib.
    let (bind_url, metadata) = match nrs_import::canonical_anime_url(&query) {
        Some(bind_url) => {
            let anime_store = state.anime_store.clone();
            tokio::task::spawn_blocking(move || {
                let store_id = anime_store.ingest(&bind_url)?;
                let metadata = anime_store.resolve(&store_id)?;
                Ok::<_, nrs_entry_store::Error>((bind_url, metadata))
            })
            .await
            .map_err(|error| store_error("run anime store ingest", error))?
            .map_err(|error| store_error("ingest through anime store", error))?
        }
        None => {
            let music_store = state.music_store.clone();
            tokio::task::spawn_blocking(move || {
                let store_id = music_store.ingest(&query)?;
                // The canonical identifier, when the store gives one.
                let bind_url = music_store
                    .lookup(&query)?
                    .and_then(|found| found.extra["identifier"].as_str().map(String::from))
                    .filter(|identifier| identifier.starts_with("http"))
                    .unwrap_or(query);
                let metadata = music_store.resolve(&store_id)?;
                Ok::<_, nrs_entry_store::Error>((bind_url, metadata))
            })
            .await
            .map_err(|error| store_error("run music store ingest", error))?
            .map_err(|error| store_error("ingest through music store", error))?
        }
    };

    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "entry ingest requires database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let id = match store
        .find_entry("url", &bind_url)
        .map_err(|error| store_error("look up ingested entry", error))?
    {
        Some(id) => id,
        None => {
            let id = store
                .add_entry("url", &bind_url, Some(&metadata.title))
                .map_err(|error| store_error("add ingested entry", error))?;
            if let Some(kind) = kind_from_store(&metadata.kind) {
                store
                    .set_entry_kind(&id, Some(kind))
                    .map_err(|error| store_error("set ingested entry kind", error))?;
            }
            id
        }
    };
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(id.0),
    }))
}

#[derive(Deserialize, Default)]
pub(super) struct ScoresQuery {
    sort: Option<String>,
    dir: Option<String>,
    /// The filter language in `score_filter`.
    #[serde(default)]
    q: String,
    offset: Option<usize>,
    limit: Option<usize>,
    /// Time decorrelation overriding the stored `TimeParams` for this
    /// view: the shared fraction `λ` and the length `L` in days.
    shared: Option<f32>,
    length: Option<f32>,
    /// Another ranking to report rank changes against (`delta`): an
    /// earlier store generation (the one the client last saw, so an edit
    /// shows what it moved) and/or other time settings. Omitted parts
    /// default to this view's. An earlier generation can only be compared
    /// while its ranking is still in `ScoresCache`.
    cmp_gen: Option<u64>,
    cmp_shared: Option<f32>,
    cmp_length: Option<f32>,
}

/// Which ranking: a store generation under some time settings.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct ScoresKey {
    generation: u64,
    shared_bits: u32,
    length_bits: u32,
}

impl ScoresKey {
    fn time(&self) -> TimeParams {
        TimeParams {
            shared: f32::from_bits(self.shared_bits),
            length_days: f32::from_bits(self.length_bits),
        }
    }
}

fn time_override(stored: TimeParams, shared: Option<f32>, length: Option<f32>) -> TimeParams {
    TimeParams {
        shared: shared.map_or(stored.shared, |v| v.clamp(0.0, 1.0)),
        length_days: length.filter(|v| *v > 0.0).unwrap_or(stored.length_days),
    }
}

/// One entry of a full ranking; `rank` 1 is the highest V.
pub(super) struct Scored {
    id: String,
    title: String,
    store: String,
    kind: &'static str,
    /// Lowercased title, id, and store, for the filter's word terms.
    haystack: String,
    value: f32,
    rank: usize,
}

/// The last few rankings computed: the current one, the ones before the
/// latest edits or time changes, and a little back-and-forth, to compare
/// against.
#[derive(Default)]
pub(super) struct ScoresCache(std::sync::Mutex<Vec<(ScoresKey, Arc<Vec<Scored>>)>>);

const SCORES_CACHE_SIZE: usize = 4;

/// The store generation and stored time settings right now.
fn current(state: &AppState) -> Result<(u64, TimeParams), (StatusCode, Json<ApiError>)> {
    match &state.store {
        Some(store) => {
            let store = store
                .lock()
                .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
            let (_, params) = store
                .load_snapshot()
                .map_err(|error| store_error("load params", error))?;
            Ok((store.generation, params.time))
        }
        None => Ok((0, TimeParams::default())),
    }
}

/// A full ranking, if it could be produced.
type MaybeRanking = Option<Arc<Vec<Scored>>>;

/// The full ranking for `key`, in rank order. Scoring every entry is the
/// expensive part of the scores view, so rankings are cached per key.
/// Only the current generation can be scored; an earlier one is served
/// from the cache or not at all.
fn scored(state: &AppState, key: ScoresKey) -> Result<MaybeRanking, (StatusCode, Json<ApiError>)> {
    if let Some(hit) = cached_ranking(state, key) {
        return Ok(Some(hit));
    }
    let rank = |snapshot: &nrs_core::Snapshot, k_feat, hints: KindHints| -> Vec<Scored> {
        rows(snapshot, SortBy::Value, SortDir::Desc, k_feat, key.time())
            .into_iter()
            .enumerate()
            .map(|(index, row)| Scored {
                id: row.id.0.clone(),
                title: row.title.to_string(),
                store: row.store.to_string(),
                kind: entry_kind(&row.id.0, row.store, &hints),
                haystack: format!("{} {} {}", row.title, row.id.0, row.store).to_lowercase(),
                value: row.value,
                rank: index + 1,
            })
            .collect()
    };
    let ranking = if let Some(store) = &state.store {
        let store = store
            .lock()
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
        if store.generation != key.generation {
            return Ok(None);
        }
        let (snapshot, params) = store
            .load_snapshot()
            .map_err(|error| store_error("load scores", error))?;
        let stored = store
            .entry_kinds()
            .map_err(|error| store_error("load entry kinds", error))?;
        drop(store);
        let hints = KindHints::new(&snapshot).with_stored(stored);
        rank(&snapshot, params.k_feat, hints)
    } else {
        let hints = KindHints::new(&state.snapshot);
        rank(
            &state.snapshot,
            state.snapshot.palette.k_feat.clone(),
            hints,
        )
    };

    let ranking = Arc::new(ranking);
    if let Ok(mut cache) = state.scores_cache.0.lock() {
        cache.retain(|(cached, _)| *cached != key);
        cache.insert(0, (key, ranking.clone()));
        cache.truncate(SCORES_CACHE_SIZE);
    }
    Ok(Some(ranking))
}

fn cached_ranking(state: &AppState, key: ScoresKey) -> Option<Arc<Vec<Scored>>> {
    let cache = state.scores_cache.0.lock().ok()?;
    cache
        .iter()
        .find(|(cached, _)| *cached == key)
        .map(|(_, ranking)| ranking.clone())
}

/// A ranking as one request sees it: the run asked for, the run it's
/// compared against (if any), and the parsed filter.
struct ScoresView {
    generation: u64,
    time: TimeParams,
    stored_time: TimeParams,
    ranking: Arc<Vec<Scored>>,
    previous_rank: Option<HashMap<String, usize>>,
    filter: ScoreFilter,
}

#[derive(Serialize)]
pub(super) struct KindCount {
    kind: &'static str,
    /// Rows this type would show, given every other filter term.
    count: usize,
    /// `"include"` or `"exclude"` when `q` has `type:` terms for it.
    state: Option<&'static str>,
    /// Those terms as written, for the UI to toggle off.
    terms: Vec<String>,
}

impl ScoresView {
    fn new(state: &AppState, query: &ScoresQuery) -> Result<Self, (StatusCode, Json<ApiError>)> {
        let (generation, stored_time) = current(state)?;
        let time = time_override(stored_time, query.shared, query.length);
        let key = ScoresKey {
            generation,
            shared_bits: time.shared.to_bits(),
            length_bits: time.length_days.to_bits(),
        };
        let ranking = scored(state, key)?.ok_or_else(|| {
            api_error(
                StatusCode::CONFLICT,
                "the store changed while scoring; retry",
            )
        })?;
        let comparing =
            query.cmp_gen.is_some() || query.cmp_shared.is_some() || query.cmp_length.is_some();
        let previous_rank = if comparing {
            let cmp_time = time_override(time, query.cmp_shared, query.cmp_length);
            let cmp_key = ScoresKey {
                generation: query.cmp_gen.unwrap_or(generation),
                shared_bits: cmp_time.shared.to_bits(),
                length_bits: cmp_time.length_days.to_bits(),
            };
            scored(state, cmp_key)?.map(|previous| {
                previous
                    .iter()
                    .map(|row| (row.id.clone(), row.rank))
                    .collect()
            })
        } else {
            None
        };
        let filter = ScoreFilter::parse(&query.q, previous_rank.is_some());
        Ok(Self {
            generation,
            time,
            stored_time,
            ranking,
            previous_rank,
            filter,
        })
    }

    /// Places `row` gained since the compared run (negative: lost).
    fn delta(&self, row: &Scored) -> Option<i64> {
        let before = *self.previous_rank.as_ref()?.get(&row.id)?;
        Some(before as i64 - row.rank as i64)
    }

    /// The matching rows as indices into the ranking, in display order
    /// (every column sorts descending unless `dir=asc`), plus how many
    /// rows each type would show.
    fn ordered(&self, query: &ScoresQuery) -> Ordered {
        let ranking = &self.ranking;
        let mut counts: HashMap<&str, usize> = HashMap::new();
        let mut order = Vec::new();
        for (index, row) in ranking.iter().enumerate() {
            let candidate = Candidate {
                haystack: &row.haystack,
                store: &row.store,
                kind: row.kind,
                value: row.value,
                rank: row.rank,
                delta: self.delta(row),
            };
            if !self.filter.matches(&candidate, true) {
                continue;
            }
            *counts.entry(row.kind).or_default() += 1;
            if self.filter.matches(&candidate, false) {
                order.push(index);
            }
        }

        // The matches re-ranked among themselves, so a filtered view
        // reads 1..N; and the same entries' order in the compared run,
        // so Δ is movement within this group rather than overall.
        let mut local: HashMap<usize, Local> = order
            .iter()
            .enumerate()
            .map(|(position, &index)| {
                let local = Local {
                    rank: position + 1,
                    delta: None,
                };
                (index, local)
            })
            .collect();
        if let Some(previous_rank) = &self.previous_rank {
            let mut before: Vec<(usize, usize)> = order
                .iter()
                .filter_map(|&index| Some((*previous_rank.get(&ranking[index].id)?, index)))
                .collect();
            before.sort_unstable();
            for (position, (_, index)) in before.into_iter().enumerate() {
                let local = local.get_mut(&index).expect("every match has a local rank");
                local.delta = Some((position + 1) as i64 - local.rank as i64);
            }
        }

        match query.sort.as_deref() {
            Some("title") => order.sort_by(|&a, &b| {
                ranking[a]
                    .title
                    .cmp(&ranking[b].title)
                    .then_with(|| ranking[a].id.cmp(&ranking[b].id))
            }),
            Some("id") => order.sort_by(|&a, &b| ranking[a].id.cmp(&ranking[b].id)),
            Some("type") => order.sort_by(|&a, &b| {
                ranking[a]
                    .kind
                    .cmp(ranking[b].kind)
                    .then_with(|| ranking[b].rank.cmp(&ranking[a].rank))
            }),
            Some("delta") => order.sort_by_key(|index| local[index].delta),
            // Ascending V is descending rank.
            _ => order.sort_by_key(|&index| std::cmp::Reverse(ranking[index].rank)),
        }
        if query.dir.as_deref() != Some("asc") {
            order.reverse();
        }
        let kinds = ENTRY_KINDS
            .iter()
            .map(|kind| {
                let (terms, excluded) = self.filter.kind_terms(kind);
                KindCount {
                    kind,
                    count: counts.get(kind).copied().unwrap_or(0),
                    state: excluded.map(|excluded| if excluded { "exclude" } else { "include" }),
                    terms: terms.into_iter().map(String::from).collect(),
                }
            })
            .collect();
        Ordered {
            order,
            kinds,
            local,
        }
    }
}

/// A row's place among the filter's matches. Equal to the overall
/// rank and Δ when nothing is filtered out.
#[derive(Clone, Copy)]
struct Local {
    rank: usize,
    delta: Option<i64>,
}

struct Ordered {
    order: Vec<usize>,
    kinds: Vec<KindCount>,
    local: HashMap<usize, Local>,
}

#[derive(Serialize)]
pub(super) struct ScoreRow {
    id: String,
    title: String,
    store: String,
    kind: &'static str,
    value: f32,
    /// Overall rank by V; filter terms like `rank<=10` use this.
    rank: usize,
    /// Places gained since the `cmp_gen` ranking (negative: lost).
    /// Absent when nothing is being compared, or for an entry that
    /// ranking lacked.
    delta: Option<i64>,
    /// `rank` and `delta` among this filter's matches only.
    filtered_rank: usize,
    filtered_delta: Option<i64>,
}

#[derive(Serialize)]
pub(super) struct ScoresPage {
    rows: Vec<ScoreRow>,
    /// The store generation this ranking was scored at; pass it back as
    /// `cmp_gen` after an edit to see what moved.
    generation: u64,
    /// Whether the comparison was honoured (an earlier generation's
    /// ranking may have left the cache).
    comparing: bool,
    /// The time settings this ranking used, and the stored defaults.
    time: TimeParams,
    stored_time: TimeParams,
    /// Rows matching `q`.
    total: usize,
    database_total: usize,
    offset: usize,
    limit: usize,
    kinds: Vec<KindCount>,
    /// Filter terms that were dropped, and why.
    warnings: Vec<String>,
}

pub(super) async fn scores(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScoresQuery>,
) -> ApiResult<ScoresPage> {
    let view = ScoresView::new(&state, &query)?;
    let Ordered {
        order,
        kinds,
        local,
    } = view.ordered(&query);
    let total = order.len();
    let offset = query.offset.unwrap_or(0).min(total);
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let rows = order[offset..(offset + limit).min(total)]
        .iter()
        .map(|&index| {
            let row = &view.ranking[index];
            ScoreRow {
                id: row.id.clone(),
                title: row.title.clone(),
                store: row.store.clone(),
                kind: row.kind,
                value: row.value,
                rank: row.rank,
                delta: view.delta(row),
                filtered_rank: local[&index].rank,
                filtered_delta: local[&index].delta,
            }
        })
        .collect();
    Ok(Json(ScoresPage {
        rows,
        generation: view.generation,
        comparing: view.previous_rank.is_some(),
        time: view.time,
        stored_time: view.stored_time,
        total,
        database_total: view.ranking.len(),
        offset,
        limit,
        kinds,
        warnings: view.filter.warnings,
    }))
}

/// The whole filtered, sorted ranking -- not paged.
pub(super) async fn scores_csv(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScoresQuery>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, Json<ApiError>)> {
    use std::fmt::Write;
    let view = ScoresView::new(&state, &query)?;
    let mut csv = String::from("id,title,store,value,type\n");
    for index in view.ordered(&query).order {
        let row = &view.ranking[index];
        writeln!(
            csv,
            "{},{},{},{:.6},{}",
            super::csv_field(&row.id),
            super::csv_field(&row.title),
            super::csv_field(&row.store),
            row.value,
            row.kind
        )
        .unwrap();
    }
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"scores.csv\"",
            ),
        ],
        csv,
    ))
}

#[derive(Serialize)]
struct PaletteAnchor {
    id: String,
    label: String,
    family: Option<String>,
}

#[derive(Serialize)]
struct PaletteCell {
    row: String,
    column: String,
    value: f32,
}

#[derive(Serialize)]
pub(super) struct PaletteResponse {
    anchors: Vec<PaletteAnchor>,
    cells: Vec<PaletteCell>,
}

pub(super) async fn palette(State(state): State<Arc<AppState>>) -> ApiResult<PaletteResponse> {
    let (snapshot, k_feat) = if let Some(store) = &state.store {
        let store = store
            .lock()
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
        let (snapshot, params) = store
            .load_snapshot()
            .map_err(|error| store_error("load palette", error))?;
        (snapshot, params.k_feat)
    } else {
        (
            state.snapshot.clone(),
            state.snapshot.palette.k_feat.clone(),
        )
    };
    let anchors = if snapshot.palette.anchors.is_empty() {
        used_anchors(&snapshot)
            .into_iter()
            .map(|id| PaletteAnchor {
                label: id.0.clone(),
                id: id.0,
                family: None,
            })
            .collect::<Vec<_>>()
    } else {
        snapshot
            .palette
            .anchors
            .iter()
            .map(|anchor| PaletteAnchor {
                id: anchor.id.0.clone(),
                label: anchor.label.clone(),
                family: anchor.family.clone(),
            })
            .collect()
    };
    let cells = anchors
        .iter()
        .flat_map(|row| {
            let k_feat = &k_feat;
            anchors.iter().map(move |column| PaletteCell {
                row: row.id.clone(),
                column: column.id.clone(),
                value: k_feat.get(
                    &nrs_core::AnchorId(row.id.clone()),
                    &nrs_core::AnchorId(column.id.clone()),
                ),
            })
        })
        .collect();
    Ok(Json(PaletteResponse { anchors, cells }))
}

#[derive(Serialize)]
struct CalibrationOption {
    id: String,
    title: String,
    value: f32,
}

#[derive(Serialize)]
pub(super) struct CalibrationPair {
    left: CalibrationOption,
    right: CalibrationOption,
}

fn calibration_pair(state: &AppState) -> Result<CalibrationPair, (StatusCode, Json<ApiError>)> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "calibration requires database mode",
        )
    })?;
    let store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let (snapshot, params) = store
        .load_snapshot()
        .map_err(|error| store_error("load calibration data", error))?;
    if snapshot.entries.len() < 2 {
        return Err(api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "need at least two entries to compare",
        ));
    }
    let scores = nrs_core::score(&snapshot, &params);
    let mut rng = rand::rng();
    let left_index = rand::Rng::random_range(&mut rng, 0..snapshot.entries.len());
    let mut right_index = rand::Rng::random_range(&mut rng, 0..snapshot.entries.len());
    while right_index == left_index {
        right_index = rand::Rng::random_range(&mut rng, 0..snapshot.entries.len());
    }
    let option = |entry: &Entry| CalibrationOption {
        id: entry.id.0.clone(),
        title: entry_display_name(entry).to_string(),
        value: scores.values.get(&entry.id).copied().unwrap_or(0.0),
    };
    Ok(CalibrationPair {
        left: option(&snapshot.entries[left_index]),
        right: option(&snapshot.entries[right_index]),
    })
}

pub(super) async fn calibration_get(
    State(state): State<Arc<AppState>>,
) -> ApiResult<CalibrationPair> {
    Ok(Json(calibration_pair(&state)?))
}

#[derive(Deserialize)]
pub(super) struct CalibrationVote {
    winner: String,
    loser: String,
}

pub(super) async fn calibration_vote(
    State(state): State<Arc<AppState>>,
    Json(vote): Json<CalibrationVote>,
) -> ApiResult<CalibrationPair> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "calibration requires database mode",
        )
    })?;
    {
        let mut store = store
            .lock()
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
        let entries = store
            .list_entries()
            .map_err(|error| store_error("load vote entries", error))?;
        if vote.winner == vote.loser
            || !entries.iter().any(|entry| entry.id.0 == vote.winner)
            || !entries.iter().any(|entry| entry.id.0 == vote.loser)
        {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "vote requires two different existing entries",
            ));
        }
        store
            .record_preference(&EntryId(vote.winner), &EntryId(vote.loser), None)
            .map_err(|error| store_error("record preference", error))?;
    }
    Ok(Json(calibration_pair(&state)?))
}

#[derive(Serialize)]
struct KfeatPair {
    a: String,
    b: String,
    value: f32,
}

/// An entry the ultrametric closure tightens: scoring already pools
/// `a` and `b` at `to`, forced by the chain `via`.
#[derive(Serialize)]
struct KfeatCorrection {
    a: String,
    b: String,
    from: f32,
    to: f32,
    via: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct KfeatResponse {
    /// Each anchor's own exponent `K(a, a)`.
    exponents: Vec<KfeatPair>,
    /// The exponent each pair pools at, `K(a, b)`.
    pairs: Vec<KfeatPair>,
    corrections: Vec<KfeatCorrection>,
}

pub(super) async fn kfeat_get(State(state): State<Arc<AppState>>) -> ApiResult<KfeatResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "k_feat requires database mode",
        )
    })?;
    let store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let (snapshot, params) = store
        .load_snapshot()
        .map_err(|error| store_error("load k_feat", error))?;
    let mut anchors = used_anchors(&snapshot);
    for anchor in &snapshot.palette.anchors {
        if !anchors.contains(&anchor.id) {
            anchors.push(anchor.id.clone());
        }
    }
    anchors.sort();
    let exponents = anchors
        .iter()
        .map(|a| KfeatPair {
            a: a.0.clone(),
            b: a.0.clone(),
            value: params.k_feat.exponent(a),
        })
        .collect();
    let corrections = nrs_core::ultrametric_closure(&params.k_feat, &anchors)
        .into_iter()
        .map(|c| KfeatCorrection {
            a: c.a.0,
            b: c.b.0,
            from: c.from,
            to: c.to,
            via: c.via.into_iter().map(|a| a.0).collect(),
        })
        .collect();
    let mut pairs = Vec::new();
    for (index, a) in anchors.iter().enumerate() {
        for b in anchors.iter().skip(index + 1) {
            pairs.push(KfeatPair {
                a: a.0.clone(),
                b: b.0.clone(),
                value: params.k_feat.get(a, b),
            });
        }
    }
    Ok(Json(KfeatResponse {
        exponents,
        pairs,
        corrections,
    }))
}

#[derive(Deserialize)]
pub(super) struct KfeatUpdate {
    a: String,
    b: String,
    value: f32,
}

#[derive(Deserialize)]
pub(super) struct TimeUpdate {
    shared: f32,
    length_days: f32,
}

/// Stores new default time settings.
pub(super) async fn time_set(
    State(state): State<Arc<AppState>>,
    Json(update): Json<TimeUpdate>,
) -> ApiResult<MutationResponse> {
    if !((0.0..=1.0).contains(&update.shared) && update.length_days > 0.0) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "time needs shared in [0, 1] and length_days > 0",
        ));
    }
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "time settings require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    store
        .set_time_params(&TimeParams {
            shared: update.shared,
            length_days: update.length_days,
        })
        .map_err(|error| store_error("save time settings", error))?;
    Ok(Json(MutationResponse { ok: true, id: None }))
}

pub(super) async fn kfeat_set(
    State(state): State<Arc<AppState>>,
    Json(update): Json<KfeatUpdate>,
) -> ApiResult<MutationResponse> {
    if !(update.value > 0.0 && update.value <= 1.0) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "a pooling exponent must lie in (0, 1]",
        ));
    }
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "k_feat requires database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    store
        .set_kfeat_pair(
            &nrs_core::AnchorId(update.a),
            &nrs_core::AnchorId(update.b),
            update.value,
        )
        .map_err(|error| store_error("save k_feat", error))?;
    Ok(Json(MutationResponse { ok: true, id: None }))
}

/// `KindHints` for the store as it is now. Building them reads every
/// relation, so they're kept until the next write.
fn kind_hints(
    state: &AppState,
    store: &TrackedStore,
) -> Result<Arc<KindHints>, (StatusCode, Json<ApiError>)> {
    let mut cache = state
        .kind_hints
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "kind cache poisoned"))?;
    if let Some((generation, hints)) = cache.as_ref() {
        if *generation == store.generation {
            return Ok(hints.clone());
        }
    }
    let (snapshot, _) = store
        .load_snapshot()
        .map_err(|error| store_error("load relations", error))?;
    let stored = store
        .entry_kinds()
        .map_err(|error| store_error("load entry kinds", error))?;
    let hints = Arc::new(KindHints::new(&snapshot).with_stored(stored));
    *cache = Some((store.generation, hints.clone()));
    Ok(hints)
}

#[derive(Serialize)]
pub(super) struct ReviewResponse {
    entries: Vec<EntrySummary>,
}

pub(super) async fn review_queue(State(state): State<Arc<AppState>>) -> ApiResult<ReviewResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "review queue requires database mode",
        )
    })?;
    let store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let hints = kind_hints(&state, &store)?;
    let flagged = store
        .entries_needing_review()
        .map_err(|error| store_error("load review queue", error))?;
    let entries = store
        .list_entries()
        .map_err(|error| store_error("load review entries", error))?;
    let lookup: HashMap<&str, &Entry> = entries
        .iter()
        .map(|entry| (entry.id.0.as_str(), entry))
        .collect();
    Ok(Json(ReviewResponse {
        entries: flagged
            .iter()
            .filter_map(|id| lookup.get(id.0.as_str()))
            .map(|entry| EntrySummary::new(entry, &hints))
            .collect(),
    }))
}

pub(super) async fn impact_confirm(
    State(state): State<Arc<AppState>>,
    Path((entry_id, impact_id)): Path<(String, String)>,
) -> ApiResult<MutationResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "impact confirmation requires database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    scoped_impact(&store, &entry_id, &impact_id)?;
    store
        .confirm_impact(&ImpactId(impact_id.clone()))
        .map_err(|error| store_error("confirm impact", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(impact_id),
    }))
}

#[derive(Clone, Serialize)]
pub(super) struct EntrySummary {
    id: String,
    title: String,
    store: String,
    store_id: String,
    kind: String,
}

impl EntrySummary {
    fn new(entry: &Entry, hints: &KindHints) -> Self {
        Self {
            id: entry.id.0.clone(),
            title: entry_display_name(entry).to_string(),
            store: entry.store.clone(),
            store_id: entry.store_id.clone(),
            kind: entry_kind(&entry.id.0, &entry.store, hints).to_string(),
        }
    }
}

#[derive(Deserialize)]
pub(super) struct EntriesQuery {
    q: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(super) struct EntriesResponse {
    entries: Vec<EntrySummary>,
    total: usize,
    database_total: usize,
    offset: usize,
    limit: usize,
}

pub(super) async fn entries_index(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EntriesQuery>,
) -> ApiResult<EntriesResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "entry API requires database mode",
        )
    })?;
    let store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let mut entries = store.list_entries().map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load entries: {error}"),
        )
    })?;
    let hints = kind_hints(&state, &store)?;
    drop(store);
    let database_total = entries.len();

    if let Some(needle) = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|needle| !needle.is_empty())
    {
        let needle = needle.to_lowercase();
        entries.retain(|entry| {
            entry.id.0.to_lowercase().contains(&needle)
                || entry_display_name(entry).to_lowercase().contains(&needle)
                || entry.store.to_lowercase().contains(&needle)
                || entry.store_id.to_lowercase().contains(&needle)
        });
    }
    entries.sort_by(|left, right| {
        entry_display_name(left)
            .cmp(entry_display_name(right))
            .then_with(|| left.id.0.cmp(&right.id.0))
    });

    let total = entries.len();
    let offset = query.offset.unwrap_or(0).min(total);
    let limit = query.limit.unwrap_or(100).clamp(1, 10_000);
    let entries = entries
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|entry| EntrySummary::new(&entry, &hints))
        .collect();
    Ok(Json(EntriesResponse {
        entries,
        total,
        database_total,
        offset,
        limit,
    }))
}

#[derive(Serialize)]
struct AnchorShare {
    anchor: String,
    share: f32,
}

#[derive(Serialize)]
struct Metric {
    label: String,
    value: String,
    progress: Option<f32>,
}

#[derive(Serialize)]
struct Period {
    from: String,
    to: String,
    caption: String,
}

#[derive(Serialize)]
struct Atom {
    source: String,
    source_entry: Option<EntrySummary>,
    blend: Vec<AnchorWeight>,
    magnitude: f32,
    time: Value,
}

#[derive(Serialize)]
struct AnchorWeight {
    anchor: String,
    weight: f32,
}

#[derive(Serialize)]
struct Impact {
    id: String,
    kind: Option<String>,
    section: &'static str,
    category: String,
    title: String,
    description: String,
    metric: Metric,
    period: Option<Period>,
    anchors: Vec<AnchorShare>,
    params: Value,
    contributor_share: Option<f32>,
    /// Set when this impact is shared between entries: every member and
    /// its share (`contributor_share` is this entry's).
    shared: Option<SharedView>,
    note: Option<String>,
    source_comment: Option<String>,
    needs_review: bool,
    atoms: Vec<Atom>,
}

#[derive(Serialize)]
struct SharedView {
    /// Sum of the shares; below 1 when part of the impact goes to no
    /// known entry.
    total: f32,
    members: Vec<SharedMemberView>,
    /// How the shares are grouped (explicit / even / weighted groups):
    /// stored if edited in the app, otherwise inferred from the shares.
    split: Option<nrs_store::SplitNode>,
    split_stored: bool,
}

#[derive(Serialize)]
struct SharedMemberView {
    id: String,
    entry: Option<EntrySummary>,
    factor: f32,
}

fn section_name(section: ImpactSection) -> &'static str {
    match section {
        ImpactSection::Profile => "profile",
        ImpactSection::Responses => "responses",
        ImpactSection::Resonance => "resonance",
        ImpactSection::Adjustments => "adjustments",
        ImpactSection::Other => "other",
    }
}

fn time_value(time: TimeSupport) -> Value {
    match time {
        TimeSupport::Moment => json!({ "type": "moment" }),
        TimeSupport::Interval { from, to } => json!({ "type": "interval", "from": from, "to": to }),
        TimeSupport::Tail {
            peak,
            lambda,
            cutoff,
        } => json!({
            "type": "tail",
            "peak": peak,
            "lambda": lambda,
            "cutoff": cutoff,
        }),
    }
}

fn impact_response(
    impact: &nrs_store::ReviewImpact,
    entries: &HashMap<&str, &Entry>,
    hints: &KindHints,
) -> Impact {
    let fields = impact
        .dah_params
        .as_deref()
        .map(parsed_dah_params)
        .unwrap_or_default();
    let kind = impact.dah_kind.as_deref();
    let (category, title, description) = impact_kind_presentation(kind);
    let (metric_label, metric_value, metric_progress) =
        impact_primary_metric(kind, &fields, impact);
    let period = impact_period_summary(kind, &fields).map(|period| Period {
        from: period.from,
        to: period.to,
        caption: period.caption,
    });
    let params = impact
        .dah_params
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_else(|| json!({}));
    Impact {
        id: impact.id.0.clone(),
        kind: impact.dah_kind.clone(),
        section: section_name(impact_section(kind)),
        category: category.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        metric: Metric {
            label: metric_label,
            value: metric_value,
            progress: metric_progress,
        },
        period,
        anchors: impact_anchor_mix(impact)
            .into_iter()
            .map(|(anchor, share)| AnchorShare { anchor, share })
            .collect(),
        params,
        contributor_share: impact.dah_factor,
        shared: (!impact.members.is_empty()).then(|| SharedView {
            total: impact.members.iter().map(|member| member.factor).sum(),
            members: impact
                .members
                .iter()
                .map(|member| SharedMemberView {
                    id: member.contributor.0.clone(),
                    entry: entries
                        .get(member.contributor.0.as_str())
                        .map(|entry| EntrySummary::new(entry, hints)),
                    factor: member.factor,
                })
                .collect(),
            split: impact.split.clone(),
            split_stored: impact.split_stored,
        }),
        note: impact.note.clone(),
        source_comment: impact.source_comment.clone(),
        needs_review: impact.needs_review,
        atoms: impact
            .atoms
            .iter()
            .map(|atom| Atom {
                source: atom.source.0.clone(),
                source_entry: entries
                    .get(atom.source.0.as_str())
                    .map(|entry| EntrySummary::new(entry, hints)),
                blend: atom
                    .blend
                    .iter()
                    .map(|(anchor, weight)| AnchorWeight {
                        anchor: anchor.0.clone(),
                        weight: *weight,
                    })
                    .collect(),
                magnitude: atom.magnitude,
                time: time_value(atom.time),
            })
            .collect(),
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RelationTransform {
    Scalar { factor: f32 },
    Regional { weights: Vec<AnchorWeight> },
    Transport { routes: Vec<AnchorRoute> },
}

#[derive(Serialize)]
struct AnchorRoute {
    from: String,
    to: String,
    weight: f32,
}

fn relation_transform(transform: &Transform) -> RelationTransform {
    match transform {
        Transform::Scalar(factor) => RelationTransform::Scalar { factor: *factor },
        Transform::Regional(weights) => RelationTransform::Regional {
            weights: weights
                .iter()
                .map(|(anchor, weight)| AnchorWeight {
                    anchor: anchor.0.clone(),
                    weight: *weight,
                })
                .collect(),
        },
        Transform::Transport(routes) => RelationTransform::Transport {
            routes: routes
                .iter()
                .map(|((from, to), weight)| AnchorRoute {
                    from: from.0.clone(),
                    to: to.0.clone(),
                    weight: *weight,
                })
                .collect(),
        },
    }
}

#[derive(Serialize)]
struct Relation {
    id: String,
    kind: &'static str,
    section: &'static str,
    label: &'static str,
    from: String,
    to: String,
    other: EntrySummary,
    transform: RelationTransform,
    params: Value,
    editable: bool,
}

fn relation_presentation(kind: RelKind, is_from: bool) -> (&'static str, &'static str) {
    match (kind, is_from) {
        (RelKind::Contains, true) => ("structure", "Contains"),
        (RelKind::Contains, false) => ("structure", "Contained by"),
        (RelKind::Credit, true) => ("authorship", "Credited on"),
        (RelKind::Credit, false) => ("authorship", "Contributors"),
        (RelKind::Remix, true) => ("lineage", "Remixed into"),
        (RelKind::Remix, false) => ("lineage", "Remix of"),
        (RelKind::FeatureMusic, true) => ("influence", "Features music"),
        (RelKind::FeatureMusic, false) => ("influence", "Featured in"),
        (RelKind::KilledBy, true) => ("influence", "Killed by"),
        (RelKind::KilledBy, false) => ("influence", "Kills"),
    }
}

fn relation_kind(kind: RelKind) -> &'static str {
    match kind {
        RelKind::Contains => "contains",
        RelKind::Credit => "credit",
        RelKind::Remix => "remix",
        RelKind::FeatureMusic => "feature_music",
        RelKind::KilledBy => "killed_by",
    }
}

/// Progress values the stores supply for an entry's `<bind url>`, which
/// the importer fills into progress impacts that leave them out.
#[derive(Serialize, Default)]
pub(super) struct ProgressBasisResponse {
    url: Option<String>,
    length_minutes: Option<f32>,
    episodes: Option<f32>,
    episode_minutes: Option<f32>,
    store_episode_minutes: Option<f32>,
}

pub(super) async fn progress_basis(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
) -> ApiResult<ProgressBasisResponse> {
    let entry = {
        let store = state.store.as_ref().ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "entry API requires database mode",
            )
        })?;
        let store = store
            .lock()
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
        store
            .list_entries()
            .map_err(|error| store_error("load entries", error))?
            .into_iter()
            .find(|entry| entry.id.0 == entry_id)
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "entry not found"))?
    };
    // Only `<bind url>` entries (store "url") have a store record.
    if entry.store != "url" {
        return Ok(Json(ProgressBasisResponse::default()));
    }
    let url = entry.store_id;
    let (music_store_url, anime_store_url) =
        (state.music_store_url.clone(), state.anime_store_url.clone());
    let lookup_url = url.clone();
    let basis = tokio::task::spawn_blocking(move || {
        let sources = nrs_import::StoreSources::from_env(&music_store_url, &anime_store_url);
        nrs_import::progress_basis(&lookup_url, &sources)
    })
    .await
    .map_err(|error| store_error("run store lookup", error))?
    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    Ok(Json(ProgressBasisResponse {
        url: Some(url),
        length_minutes: basis.length_minutes,
        episodes: basis.episodes,
        episode_minutes: Some(basis.episode_minutes),
        store_episode_minutes: basis.store_episode_minutes,
    }))
}

/// The music store's tracklist for an album entry, each track with the
/// `contains` relations from this entry that bind it.
#[derive(Serialize, Default)]
pub(super) struct TracklistResponse {
    tracks: Vec<TracklistTrack>,
}

#[derive(Serialize)]
struct TracklistTrack {
    disc: Option<i64>,
    track: Option<i64>,
    title: Option<String>,
    /// Empty when no `contains` from this entry binds the track.
    relation_ids: Vec<String>,
}

pub(super) async fn entry_tracklist(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
) -> ApiResult<TracklistResponse> {
    let (album_url, children) = {
        let store = state.store.as_ref().ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "entry API requires database mode",
            )
        })?;
        let store = store
            .lock()
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
        let entries = store
            .list_entries()
            .map_err(|error| store_error("load entries", error))?;
        let bind_url = |id: &str| {
            entries
                .iter()
                .find(|entry| entry.id.0 == id && entry.store == "url")
                .map(|entry| entry.store_id.clone())
        };
        let album_url = bind_url(&entry_id);
        let children: Vec<(String, String)> = store
            .relations_for_entry(&EntryId(entry_id.clone()))
            .map_err(|error| store_error("load relations", error))?
            .into_iter()
            .filter(|relation| relation.kind == RelKind::Contains && relation.from.0 == entry_id)
            .filter_map(|relation| Some((relation.id, bind_url(&relation.to.0)?)))
            .collect();
        (album_url, children)
    };
    let (Some(album_url), Some(music_store_url)) = (album_url, state.music_store_url.clone())
    else {
        return Ok(Json(TracklistResponse::default()));
    };
    let tracks =
        tokio::task::spawn_blocking(move || tracklist(&music_store_url, &album_url, &children))
            .await
            .map_err(|error| store_error("run store lookup", error))?
            .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    Ok(Json(TracklistResponse { tracks }))
}

/// `resolve`'s `extra.tracklist` for `album_url`, with `children`
/// (`contains` relation id, bind URL) placed on the tracks they bind.
fn tracklist(
    music_store_url: &str,
    album_url: &str,
    children: &[(String, String)],
) -> Result<Vec<TracklistTrack>, String> {
    let err = |e: nrs_entry_store::Error| format!("music store {music_store_url}: {e}");
    let store = nrs_entry_store::HttpEntryStore::new(music_store_url)
        .map_err(err)?
        .expecting_store("musiclib");
    let Some(album) = store.lookup(album_url).map_err(err)? else {
        return Ok(Vec::new());
    };
    let meta = store.resolve(&album.store_id).map_err(err)?;
    let Some(listed) = meta.extra["tracklist"].as_array() else {
        return Ok(Vec::new());
    };
    let mut relations_of: HashMap<String, Vec<String>> = HashMap::new();
    for (relation_id, url) in children {
        if let Some(track) = store.lookup(url).map_err(err)? {
            relations_of
                .entry(track.store_id)
                .or_default()
                .push(relation_id.clone());
        }
    }
    Ok(listed
        .iter()
        .map(|track| TracklistTrack {
            disc: track["disc"].as_i64(),
            track: track["track"].as_i64(),
            title: track["title"].as_str().map(str::to_string),
            relation_ids: track["store_id"]
                .as_str()
                .and_then(|id| relations_of.remove(id))
                .unwrap_or_default(),
        })
        .collect())
}

#[derive(Serialize)]
pub(super) struct EntryResponse {
    entry: EntrySummary,
    /// The kind with no stored one, and whether one is stored.
    inferred_kind: &'static str,
    kind_stored: bool,
    impacts: Vec<Impact>,
    relations: Vec<Relation>,
}

pub(super) async fn entry_detail(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
) -> ApiResult<EntryResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "entry API requires database mode",
        )
    })?;
    let store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let entries = store.list_entries().map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load entries: {error}"),
        )
    })?;
    let entry = entries
        .iter()
        .find(|entry| entry.id.0 == entry_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "entry not found"))?;
    let impacts = store
        .impacts_for_entry(&EntryId(entry_id.clone()))
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load impacts: {error}"),
            )
        })?;
    let relations = store
        .relations_for_entry(&EntryId(entry_id.clone()))
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load relations: {error}"),
            )
        })?;
    let hints = kind_hints(&state, &store)?;
    drop(store);

    let lookup: HashMap<&str, &Entry> = entries
        .iter()
        .map(|entry| (entry.id.0.as_str(), entry))
        .collect();
    let relation_responses = relations
        .into_iter()
        .filter_map(|relation| {
            let is_from = relation.from.0 == entry_id;
            let other_id = if is_from {
                relation.to.0.as_str()
            } else {
                relation.from.0.as_str()
            };
            let other = lookup.get(other_id)?;
            let (section, label) = relation_presentation(relation.kind, is_from);
            let params = relation_params(
                relation.kind,
                &relation.transform,
                relation.dah_params.as_deref(),
            );
            Some(Relation {
                id: relation.id,
                kind: relation_kind(relation.kind),
                section,
                label,
                from: relation.from.0,
                to: relation.to.0,
                other: EntrySummary::new(other, &hints),
                editable: params.is_some(),
                transform: relation_transform(&relation.transform),
                params: params.unwrap_or_else(|| json!({})),
            })
        })
        .collect();
    Ok(Json(EntryResponse {
        entry: EntrySummary::new(entry, &hints),
        inferred_kind: inferred_kind(&entry.id.0, &entry.store, &hints),
        kind_stored: hints.stored.contains_key(&entry.id.0),
        impacts: impacts
            .iter()
            .map(|impact| impact_response(impact, &lookup, &hints))
            .collect(),
        relations: relation_responses,
    }))
}

#[derive(Deserialize)]
pub(super) struct KindUpdate {
    /// One of `ENTRY_KINDS`; `None` goes back to the inferred kind.
    kind: Option<String>,
}

pub(super) async fn entry_kind_set(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
    Json(update): Json<KindUpdate>,
) -> ApiResult<MutationResponse> {
    let kind = match update.kind.as_deref() {
        None => None,
        Some(kind) => Some(entry_kind_slug(kind).ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                format!("unknown entry kind {kind:?}"),
            )
        })?),
    };
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "entry writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    store
        .set_entry_kind(&EntryId(entry_id.clone()), kind)
        .map_err(|error| store_error("set entry kind", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(entry_id),
    }))
}

#[derive(Deserialize)]
pub(super) struct RawImpactUpdate {
    note: String,
    atoms: Vec<nrs_core::Atom>,
}

pub(super) async fn impact_raw_edit(
    State(state): State<Arc<AppState>>,
    Path((entry_id, impact_id)): Path<(String, String)>,
    Json(update): Json<RawImpactUpdate>,
) -> ApiResult<MutationResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "impact writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let existing = scoped_impact(&store, &entry_id, &impact_id)?;
    if !existing.members.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "this impact is shared between entries; edit its split with the share editor",
        ));
    }
    if existing.dah_kind.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "use the standards editor for this impact",
        ));
    }
    if update.atoms.len() != existing.atoms.len()
        || update.atoms.iter().any(|atom| {
            !atom.magnitude.is_finite()
                || atom.source.0.trim().is_empty()
                || atom
                    .blend
                    .iter()
                    .any(|(anchor, weight)| anchor.0.trim().is_empty() || !weight.is_finite())
        })
    {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid source atoms"));
    }
    // Keep imported time supports intact; the review editor changes source/blend/magnitude only.
    let mut atoms = update.atoms;
    for (atom, original) in atoms.iter_mut().zip(&existing.atoms) {
        atom.time = original.time;
    }
    let note = update.note.trim();
    store
        .edit_impact(
            &ImpactId(impact_id.clone()),
            &atoms,
            (!note.is_empty()).then_some(note),
        )
        .map_err(|error| store_error("save raw impact", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(impact_id),
    }))
}

fn prepare_impact(
    entry_id: &str,
    mut fields: HashMap<String, String>,
) -> Result<
    (
        String,
        Vec<nrs_core::Atom>,
        Option<String>,
        f32,
        HashMap<String, String>,
    ),
    (StatusCode, Json<ApiError>),
> {
    let kind = fields.get("kind").cloned().unwrap_or_default();
    if kind.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "impact kind is required",
        ));
    }
    normalize_period_fields(&kind, &mut fields)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    normalize_duration_fields(&kind, &mut fields)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    let source = fields
        .get("source")
        .cloned()
        .filter(|source| !source.trim().is_empty())
        .unwrap_or_else(|| entry_id.to_string());
    fields.insert("source".into(), source.clone());
    let mut atoms = build_impact_atoms(&kind, SourceId(source), &fields)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    if matches!(kind.as_str(), "pads" | "waifu" | "meme") {
        if let (Some(from), Some(to)) = (fields.get("period_from"), fields.get("period_to")) {
            atoms = nrs_import::dated(atoms, from, to);
        }
    }
    let factor = dah_factor_field(&fields);
    for atom in &mut atoms {
        atom.magnitude *= factor;
    }
    let note = fields
        .get("note")
        .map(|note| note.trim())
        .filter(|note| !note.is_empty())
        .map(str::to_owned);
    Ok((kind, atoms, note, factor, fields))
}

pub(super) async fn impact_add(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
    Json(fields): Json<HashMap<String, String>>,
) -> ApiResult<MutationResponse> {
    let (kind, atoms, note, factor, fields) = prepare_impact(&entry_id, fields)?;
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "impact writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let id = store
        .add_impact(&EntryId(entry_id), &atoms, note.as_deref())
        .map_err(|error| store_error("add impact", error))?;
    if let Err(error) = store.set_impact_dah(&id, &kind, &dah_params_json(&fields), factor) {
        // Avoid leaving a half-created row if the presentation metadata write fails.
        let _ = store.delete_impact(&id);
        return Err(store_error("save impact metadata", error));
    }
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(id.0),
    }))
}

pub(super) async fn impact_edit(
    State(state): State<Arc<AppState>>,
    Path((entry_id, impact_id)): Path<(String, String)>,
    Json(mut fields): Json<HashMap<String, String>>,
) -> ApiResult<MutationResponse> {
    // The split of a shared impact has its own editor; this form only
    // changes its values, computed unscaled (share 1).
    fields.remove("shares");
    let unscaled = {
        let mut fields = fields.clone();
        fields.insert("dah_factor".into(), "1".into());
        prepare_impact(&entry_id, fields)?
    };
    let (kind, atoms, note, factor, fields) = prepare_impact(&entry_id, fields)?;
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "impact writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let id = ImpactId(impact_id.clone());
    let existing = scoped_impact(&store, &entry_id, &impact_id)?;
    if existing.dah_kind.is_none() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "use the raw editor for an impact without standards metadata",
        ));
    }
    if !existing.members.is_empty() {
        let (kind, atoms, note, _, fields) = unscaled;
        store
            .edit_shared_values(&id, &atoms, note.as_deref())
            .map_err(|error| store_error("edit shared impact", error))?;
        store
            .set_impact_dah(&id, &kind, &dah_params_json(&fields), 1.0)
            .map_err(|error| store_error("save impact metadata", error))?;
        return Ok(Json(MutationResponse {
            ok: true,
            id: Some(impact_id),
        }));
    }
    store
        .edit_impact(&id, &atoms, note.as_deref())
        .map_err(|error| store_error("edit impact", error))?;
    store
        .set_impact_dah(&id, &kind, &dah_params_json(&fields), factor)
        .map_err(|error| store_error("save impact metadata", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(impact_id),
    }))
}

pub(super) async fn impact_delete(
    State(state): State<Arc<AppState>>,
    Path((entry_id, impact_id)): Path<(String, String)>,
) -> ApiResult<MutationResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "impact writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    // A shared impact goes as a whole, from any member's page; dropping a
    // single share is an edit of the share list.
    scoped_impact(&store, &entry_id, &impact_id)?;
    store
        .delete_impact(&ImpactId(impact_id.clone()))
        .map_err(|error| store_error("delete impact", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(impact_id),
    }))
}

#[derive(Deserialize)]
struct ShareInput {
    contributor: String,
    factor: f32,
}

/// Validates a shared impact's split: every contributor a known entry,
/// every share in 0..=1.
fn shares_to_members(
    store: &nrs_store::Store,
    shares: Vec<ShareInput>,
) -> Result<Vec<nrs_store::ImpactMember>, (StatusCode, Json<ApiError>)> {
    let known: std::collections::HashSet<String> = store
        .list_entries()
        .map_err(|error| store_error("load entries", error))?
        .into_iter()
        .map(|entry| entry.id.0)
        .collect();
    let mut members = Vec::with_capacity(shares.len());
    for share in shares {
        if !known.contains(&share.contributor) {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!("unknown entry {}", share.contributor),
            ));
        }
        if !share.factor.is_finite() || !(0.0..=1.0).contains(&share.factor) {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!("share for {} must be between 0 and 1", share.contributor),
            ));
        }
        members.push(nrs_store::ImpactMember {
            contributor: EntryId(share.contributor),
            factor: share.factor,
        });
    }
    Ok(members)
}

/// A split as the editor sends it: a tree of groups, or flat shares.
#[derive(Deserialize)]
pub(super) struct SharesUpdate {
    split: Option<nrs_store::SplitNode>,
    shares: Option<Vec<ShareInput>>,
}

/// Turns an ordinary impact into a shared one (this entry as its only
/// member), so its split editor can add more entries.
pub(super) async fn impact_make_shared(
    State(state): State<Arc<AppState>>,
    Path((entry_id, impact_id)): Path<(String, String)>,
) -> ApiResult<MutationResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "impact writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let existing = scoped_impact(&store, &entry_id, &impact_id)?;
    if !existing.members.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "this impact is already shared",
        ));
    }
    store
        .make_impact_shared(&ImpactId(impact_id.clone()))
        .map_err(|error| store_error("share impact", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(impact_id),
    }))
}

/// Changes only a shared impact's split, keeping its values and note.
pub(super) async fn impact_shares_edit(
    State(state): State<Arc<AppState>>,
    Path((entry_id, impact_id)): Path<(String, String)>,
    Json(update): Json<SharesUpdate>,
) -> ApiResult<MutationResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "impact writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    scoped_impact(&store, &entry_id, &impact_id)?;
    let id = ImpactId(impact_id.clone());
    let shared = store
        .shared_impact(&id)
        .map_err(|error| store_error("load shared impact", error))?
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "this impact is not shared"))?;
    match (update.split, update.shares) {
        (Some(tree), _) => {
            // Every member must be a known entry; the store checks the rest.
            let flat = nrs_store::flatten_split(&tree);
            shares_to_members(
                &store,
                flat.into_iter()
                    .map(|member| ShareInput {
                        contributor: member.contributor.0,
                        factor: member.factor.clamp(0.0, 1.0),
                    })
                    .collect(),
            )?;
            store
                .set_impact_split(&id, &tree)
                .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
        }
        (None, Some(shares)) => {
            let members = shares_to_members(&store, shares)?;
            store
                .edit_shared_impact(&id, &shared.atoms, &members, shared.note.as_deref())
                .map_err(|error| store_error("edit shares", error))?;
        }
        (None, None) => {
            return Err(api_error(StatusCode::BAD_REQUEST, "send a split or shares"));
        }
    }
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(impact_id),
    }))
}

/// A relation's standards parameters: the stored form, or, for an imported
/// relation (none stored), what its transform implies. `None` when it can't
/// be edited as a form (a credit without its roles expression).
fn relation_params(
    kind: RelKind,
    transform: &Transform,
    dah_params: Option<&str>,
) -> Option<Value> {
    let stored = dah_params.and_then(|params| serde_json::from_str::<Value>(params).ok());
    if kind == RelKind::Credit {
        return stored.filter(|params| params["roles"].is_string());
    }
    if stored.is_some() {
        return stored;
    }
    match (kind, transform) {
        (RelKind::Contains, Transform::Scalar(factor)) => {
            Some(json!({ "factor": factor.to_string() }))
        }
        (RelKind::Remix | RelKind::FeatureMusic, _) => Some(json!({})),
        // `killed_by` scales by potential × effect only, so the product is
        // all a transform keeps: shown as the potential, at full effect.
        (RelKind::KilledBy, Transform::Regional(map)) => {
            map.get(&nrs_core::AnchorId("comedy".into())).map(|comedy| {
                let product = (comedy / (0.2 * 0.4) * 10_000.0).round() / 10_000.0;
                json!({ "potential": product.to_string(), "effect": "1" })
            })
        }
        _ => None,
    }
}

/// A credit's transform from its roles expression, plus the work's other
/// credits that change with it. The work's music variables aren't stored,
/// so they're recovered from its existing credits; featuring follows the
/// roles (an `image_feat` role anywhere on the work), unless the work had
/// it pinned. `replacing` is the credit being edited, if any.
#[allow(clippy::type_complexity)]
fn credit_writes(
    store: &nrs_store::Store,
    replacing: Option<&str>,
    work: &EntryId,
    roles: &str,
) -> Result<(Transform, Vec<(nrs_store::EntryRelation, Transform)>), (StatusCode, Json<ApiError>)> {
    use nrs_import::roles;
    let terms =
        roles::parse_credit_roles(roles).map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
    let siblings: Vec<nrs_store::EntryRelation> = store
        .relations_for_entry(work)
        .map_err(|error| store_error("load credits", error))?
        .into_iter()
        .filter(|r| r.kind == RelKind::Credit && &r.to == work && Some(r.id.as_str()) != replacing)
        .collect();
    let mut stored: Vec<(String, Transform)> = Vec::new();
    let mut others = Vec::new();
    for sibling in &siblings {
        let roles = relation_params(
            sibling.kind,
            &sibling.transform,
            sibling.dah_params.as_deref(),
        )
        .and_then(|p| p["roles"].as_str().map(String::from))
        .ok_or_else(|| {
            api_error(
                StatusCode::CONFLICT,
                "another credit on this work has no roles expression",
            )
        })?;
        stored.push((roles.clone(), sibling.transform.clone()));
        others.push((sibling, roles));
    }
    if let Some(id) = replacing {
        if let Some(old) = store
            .relations_for_entry(work)
            .map_err(|error| store_error("load credits", error))?
            .into_iter()
            .find(|r| r.id == id && &r.to == work)
        {
            if let Some(roles) =
                relation_params(old.kind, &old.transform, old.dah_params.as_deref())
                    .and_then(|p| p["roles"].as_str().map(String::from))
            {
                stored.push((roles, old.transform));
            }
        }
    }
    let vars = if stored.is_empty() {
        roles::MusicVars::default()
    } else {
        roles::infer_music_vars(&stored).ok_or_else(|| {
            api_error(
                StatusCode::CONFLICT,
                "couldn't recover this work's music variables from its credits",
            )
        })?
    };
    let mentions = |roles: &str| {
        roles::parse_credit_roles(roles).is_ok_and(|t| roles::mentions_image_feat(&t))
    };
    let pinned = vars.feat && !stored.iter().any(|(r, _)| mentions(r));
    let feat =
        pinned || roles::mentions_image_feat(&terms) || others.iter().any(|(_, r)| mentions(r));
    let vars = roles::MusicVars { feat, ..vars };
    let transform = roles::vector_to_transform(roles::weight_vector(&terms, &vars));
    let mut updates = Vec::new();
    for (sibling, roles) in others {
        let next = roles::credit_transform(&roles, &vars)
            .map_err(|e| api_error(StatusCode::CONFLICT, e))?;
        if next != sibling.transform {
            updates.push((sibling.clone(), next));
        }
    }
    Ok((transform, updates))
}

/// Writes a credit (new when `id` is `None`) and refreshes the work's
/// other credits when featuring flips.
fn write_credit(
    store: &mut nrs_store::Store,
    id: Option<&str>,
    fields: &HashMap<String, String>,
) -> Result<String, (StatusCode, Json<ApiError>)> {
    let from = EntryId(fields.get("from_entry").cloned().unwrap_or_default());
    let to = EntryId(fields.get("to_entry").cloned().unwrap_or_default());
    let roles = fields
        .get("roles")
        .map(|r| r.trim().to_string())
        .unwrap_or_default();
    let (transform, updates) = credit_writes(store, id, &to, &roles)?;
    let id = match id {
        Some(id) => {
            store
                .edit_relation(id, RelKind::Credit, &from, &to, &transform)
                .map_err(|error| store_error("edit credit", error))?;
            id.to_string()
        }
        None => store
            .add_relation(RelKind::Credit, &from, &to, &transform)
            .map_err(|error| store_error("add credit", error))?,
    };
    store
        .set_relation_dah(&id, &json!({ "roles": roles }).to_string())
        .map_err(|error| store_error("save credit roles", error))?;
    for (sibling, transform) in updates {
        store
            .edit_relation(
                &sibling.id,
                sibling.kind,
                &sibling.from,
                &sibling.to,
                &transform,
            )
            .map_err(|error| store_error("refresh credit", error))?;
    }
    Ok(id)
}

fn check_relation_ends(
    fields: &HashMap<String, String>,
    entry_id: &str,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let end = |name: &str| fields.get(name).map(|v| v.trim()).unwrap_or("");
    if end("from_entry").is_empty() || end("to_entry").is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "from_entry and to_entry are required",
        ));
    }
    if end("from_entry") != entry_id && end("to_entry") != entry_id {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "relation must touch this entry",
        ));
    }
    Ok(())
}

fn relation_from_fields(
    fields: &HashMap<String, String>,
) -> Result<nrs_core::Relation, (StatusCode, Json<ApiError>)> {
    let kind = fields.get("kind").cloned().unwrap_or_default();
    let from = fields.get("from_entry").cloned().unwrap_or_default();
    let to = fields.get("to_entry").cloned().unwrap_or_default();
    if from.trim().is_empty() || to.trim().is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "from_entry and to_entry are required",
        ));
    }
    build_relation(&kind, EntryId(from), EntryId(to), fields)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))
}

pub(super) async fn relation_add(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
    Json(fields): Json<HashMap<String, String>>,
) -> ApiResult<MutationResponse> {
    if fields.get("kind").map(String::as_str) == Some("credit") {
        let store = state.store.as_ref().ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "relation writes require database mode",
            )
        })?;
        let mut store = store
            .lock()
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
        check_relation_ends(&fields, &entry_id)?;
        let id = write_credit(&mut store, None, &fields)?;
        return Ok(Json(MutationResponse {
            ok: true,
            id: Some(id),
        }));
    }
    let relation = relation_from_fields(&fields)?;
    if relation.from.0 != entry_id && relation.to.0 != entry_id {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "relation must touch this entry",
        ));
    }
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relation writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let id = store
        .add_relation(
            relation.kind,
            &relation.from,
            &relation.to,
            &relation.transform,
        )
        .map_err(|error| store_error("add relation", error))?;
    if let Err(error) = store.set_relation_dah(&id, &dah_params_json(&fields)) {
        let _ = store.delete_relation(&id);
        return Err(store_error("save relation metadata", error));
    }
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(id),
    }))
}

pub(super) async fn relation_edit(
    State(state): State<Arc<AppState>>,
    Path((entry_id, relation_id)): Path<(String, String)>,
    Json(fields): Json<HashMap<String, String>>,
) -> ApiResult<MutationResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relation writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    let existing = scoped_relation(&store, &entry_id, &relation_id)?;
    if relation_params(
        existing.kind,
        &existing.transform,
        existing.dah_params.as_deref(),
    )
    .is_none()
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "this imported relation has no editable standards parameters",
        ));
    }
    if fields.get("kind").map(String::as_str) == Some("credit") {
        check_relation_ends(&fields, &entry_id)?;
        let id = write_credit(&mut store, Some(&relation_id), &fields)?;
        return Ok(Json(MutationResponse {
            ok: true,
            id: Some(id),
        }));
    }
    let relation = relation_from_fields(&fields)?;
    if relation.from.0 != entry_id && relation.to.0 != entry_id {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "relation must touch this entry",
        ));
    }
    store
        .edit_relation(
            &relation_id,
            relation.kind,
            &relation.from,
            &relation.to,
            &relation.transform,
        )
        .map_err(|error| store_error("edit relation", error))?;
    store
        .set_relation_dah(&relation_id, &dah_params_json(&fields))
        .map_err(|error| store_error("save relation metadata", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(relation_id),
    }))
}

pub(super) async fn relation_delete(
    State(state): State<Arc<AppState>>,
    Path((entry_id, relation_id)): Path<(String, String)>,
) -> ApiResult<MutationResponse> {
    let store = state.store.as_ref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relation writes require database mode",
        )
    })?;
    let mut store = store
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
    scoped_relation(&store, &entry_id, &relation_id)?;
    store
        .delete_relation(&relation_id)
        .map_err(|error| store_error("delete relation", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(relation_id),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> (Arc<AppState>, String, String, String) {
        let mut store = nrs_store::Store::open_in_memory().unwrap();
        let a = store.add_entry("none", "a", Some("A")).unwrap();
        let b = store.add_entry("none", "b", Some("B")).unwrap();
        let impact = store
            .add_impact(
                &a,
                &[nrs_core::Atom {
                    source: SourceId(a.0.clone()),
                    blend: vec![(nrs_core::AnchorId("music".into()), 1.0)],
                    magnitude: 2.0,
                    time: TimeSupport::Tail {
                        peak: 1.0,
                        lambda: 0.5,
                        cutoff: 10.0,
                    },
                }],
                None,
            )
            .unwrap();
        let snapshot = store.load_snapshot().unwrap().0;
        (
            Arc::new(AppState {
                snapshot,
                store: Some(std::sync::Mutex::new(crate::TrackedStore::new(store))),
                anime_store: nrs_entry_store::HttpEntryStore::new("http://127.0.0.1:3738").unwrap(),
                music_store: nrs_entry_store::HttpEntryStore::new("http://127.0.0.1:4600").unwrap(),
                music_store_url: None,
                anime_store_url: None,
                scores_cache: Default::default(),
                kind_hints: Default::default(),
            }),
            a.0,
            b.0,
            impact.0,
        )
    }

    #[tokio::test]
    async fn mutations_cannot_address_another_entries_impact() {
        let (state, a, b, id) = test_state();
        let result = impact_delete(State(state.clone()), Path((b, id.clone()))).await;
        assert_eq!(result.err().unwrap().0, StatusCode::NOT_FOUND);
        let store = state.store.as_ref().unwrap().lock().unwrap();
        assert_eq!(store.impacts_for_entry(&EntryId(a)).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deleting_a_shared_impact_from_a_member_deletes_it_for_all() {
        let (state, a, b, id) = test_state();
        {
            let mut store = state.store.as_ref().unwrap().lock().unwrap();
            let impact = ImpactId(id.clone());
            store.make_impact_shared(&impact).unwrap();
            let atoms = store.impacts_for_entry(&EntryId(a.clone())).unwrap()[0]
                .atoms
                .clone();
            let members = [&a, &b].map(|e| nrs_store::ImpactMember {
                contributor: EntryId(e.clone()),
                factor: 0.5,
            });
            store
                .edit_shared_impact(&impact, &atoms, &members, None)
                .unwrap();
        }
        let result = impact_delete(State(state.clone()), Path((b.clone(), id))).await;
        assert!(result.is_ok());
        let store = state.store.as_ref().unwrap().lock().unwrap();
        assert!(store.impacts_for_entry(&EntryId(a)).unwrap().is_empty());
        assert!(store.impacts_for_entry(&EntryId(b)).unwrap().is_empty());
    }

    #[tokio::test]
    async fn raw_review_edits_preserve_time_support_and_provenance() {
        let (state, a, _, id) = test_state();
        let result = impact_raw_edit(
            State(state.clone()),
            Path((a.clone(), id)),
            Json(RawImpactUpdate {
                note: " reviewed ".into(),
                atoms: vec![nrs_core::Atom {
                    source: SourceId(a.clone()),
                    blend: vec![(nrs_core::AnchorId("writing".into()), 0.7)],
                    magnitude: 3.0,
                    time: TimeSupport::Moment,
                }],
            }),
        )
        .await;
        assert!(result.is_ok());
        let store = state.store.as_ref().unwrap().lock().unwrap();
        let impact = store.impacts_for_entry(&EntryId(a)).unwrap().remove(0);
        assert_eq!(impact.note.as_deref(), Some("reviewed"));
        assert!(impact.dah_kind.is_none());
        assert_eq!(impact.atoms[0].magnitude, 3.0);
        assert_eq!(
            impact.atoms[0].time,
            TimeSupport::Tail {
                peak: 1.0,
                lambda: 0.5,
                cutoff: 10.0
            }
        );
    }

    #[tokio::test]
    async fn scores_reload_edits_instead_of_using_startup_snapshot() {
        let (state, a, _, id) = test_state();
        let query = || Query(ScoresQuery::default());
        let before = scores(State(state.clone()), query()).await.ok().unwrap().0;
        let _ = impact_delete(State(state.clone()), Path((a.clone(), id)))
            .await
            .ok()
            .unwrap();
        // Same parameters, so this would be a cache hit if the write
        // didn't invalidate it.
        let after = scores(State(state), query()).await.ok().unwrap().0;
        assert!(before.rows.iter().find(|row| row.id == a).unwrap().value > 0.0);
        assert_eq!(
            after.rows.iter().find(|row| row.id == a).unwrap().value,
            0.0
        );
    }

    #[tokio::test]
    async fn scores_page_sorts_filters_and_compares() {
        let (state, a, b, _) = test_state();
        let page = |query: ScoresQuery| scores(State(state.clone()), Query(query));

        let all = page(ScoresQuery::default()).await.ok().unwrap().0;
        assert_eq!((all.total, all.database_total), (2, 2));
        // `a` is the only entry with an impact, so it ranks first.
        assert_eq!((all.rows[0].id.as_str(), all.rows[0].rank), (a.as_str(), 1));
        assert!(all.rows.iter().all(|row| row.delta.is_none()));

        let second = page(ScoresQuery {
            offset: Some(1),
            limit: Some(1),
            ..Default::default()
        })
        .await
        .ok()
        .unwrap()
        .0;
        assert_eq!(second.rows.len(), 1);
        assert_eq!(second.rows[0].id, b);

        // Sorting changes the order but not the rank.
        let by_title = page(ScoresQuery {
            sort: Some("title".into()),
            dir: Some("desc".into()),
            ..Default::default()
        })
        .await
        .ok()
        .unwrap()
        .0;
        assert_eq!(
            (by_title.rows[0].id.as_str(), by_title.rows[0].rank),
            (b.as_str(), 2)
        );

        let none = page(ScoresQuery {
            q: "no such title".into(),
            ..Default::default()
        })
        .await
        .ok()
        .unwrap()
        .0;
        assert_eq!(
            (none.total, none.database_total, none.rows.len()),
            (0, 2, 0)
        );

        let generation = page(ScoresQuery::default())
            .await
            .ok()
            .unwrap()
            .0
            .generation;
        let compared = page(ScoresQuery {
            cmp_gen: Some(generation),
            ..Default::default()
        })
        .await
        .ok()
        .unwrap()
        .0;
        assert!(compared.rows.iter().all(|row| row.delta == Some(0)));

        // Time settings override the stored defaults per view, and can be
        // compared against each other.
        let knobs = page(ScoresQuery {
            shared: Some(1.0),
            length: Some(90.0),
            cmp_shared: Some(0.5),
            ..Default::default()
        })
        .await
        .ok()
        .unwrap()
        .0;
        assert_eq!((knobs.time.shared, knobs.time.length_days), (1.0, 90.0));
        assert_eq!(knobs.stored_time, TimeParams::default());
        assert!(knobs.comparing);

        // A filtered view ranks its matches 1..N, keeping the overall
        // rank alongside.
        let filtered = page(ScoresQuery {
            q: "rank>=2".into(),
            cmp_gen: Some(generation),
            ..Default::default()
        })
        .await
        .ok()
        .unwrap()
        .0;
        let row = &filtered.rows[0];
        assert_eq!(
            (row.id.as_str(), row.rank, row.filtered_rank),
            (b.as_str(), 2, 1)
        );
        assert_eq!(row.filtered_delta, Some(0));
    }

    #[tokio::test]
    async fn imported_relations_edit_from_their_transform_not_defaults() {
        let (state, a, b, _) = test_state();
        let id = state
            .store
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .add_relation(
                RelKind::Contains,
                &EntryId(a.clone()),
                &EntryId(b.clone()),
                &Transform::Scalar(0.37),
            )
            .unwrap();
        // The form starts from the stored weight, not the default 1.
        assert_eq!(
            relation_params(RelKind::Contains, &Transform::Scalar(0.37), None),
            Some(json!({ "factor": "0.37" }))
        );
        relation_edit(
            State(state.clone()),
            Path((a.clone(), id)),
            Json(HashMap::from([
                ("kind".into(), "contains".into()),
                ("from_entry".into(), a.clone()),
                ("to_entry".into(), b),
                ("factor".into(), "0.5".into()),
            ])),
        )
        .await
        .unwrap_or_else(|(_, error)| panic!("{}", error.0.error));
        let store = state.store.as_ref().unwrap().lock().unwrap();
        assert_eq!(
            store.relations_for_entry(&EntryId(a)).unwrap()[0].transform,
            Transform::Scalar(0.5)
        );
    }

    #[test]
    fn killed_by_params_rebuild_the_same_transform() {
        let original = nrs_import::killed_by(EntryId("a".into()), EntryId("b".into()), 0.5, 0.4);
        let params = relation_params(RelKind::KilledBy, &original.transform, None).unwrap();
        let fields: HashMap<String, String> = params
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        let rebuilt = build_relation(
            "killed_by",
            EntryId("a".into()),
            EntryId("b".into()),
            &fields,
        )
        .unwrap();
        assert_eq!(rebuilt.transform, original.transform);
    }

    #[test]
    fn remix_api_direction_matches_legacy_source_to_derived_edges() {
        assert_eq!(
            relation_presentation(RelKind::Remix, true),
            ("lineage", "Remixed into")
        );
        assert_eq!(
            relation_presentation(RelKind::Remix, false),
            ("lineage", "Remix of")
        );
    }
}
