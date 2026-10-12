//! Versioned JSON API for the dependency-free browser client.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use nrs_core::{Entry, EntryId, ImpactId, RelKind, SourceId, TimeSupport, Transform};
use nrs_entry_store::EntryStore as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::score_filter::{Candidate, ScoreFilter};
use super::{
    build_impact_atoms, build_relation, dah_factor_field, dah_params_json, entry_display_name,
    entry_kind, impact_anchor_mix, impact_kind_presentation, impact_period_summary,
    impact_primary_metric, impact_section, normalize_duration_fields, normalize_period_fields,
    parsed_dah_params, rows, used_anchors, AppState, ImpactSection, KindHints, Mode, RelScaling,
    SortBy, SortDir, TrackedStore, ENTRY_KINDS,
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
    if query.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "ingest query must not be empty",
        ));
    }
    let anime_store = state.anime_store.clone();
    let (store_id, metadata) = tokio::task::spawn_blocking(move || {
        let store_id = anime_store.ingest(&query)?;
        let metadata = anime_store.resolve(&store_id)?;
        Ok::<_, nrs_entry_store::Error>((store_id, metadata))
    })
    .await
    .map_err(|error| store_error("run anime store ingest", error))?
    .map_err(|error| store_error("ingest through anime store", error))?;

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
        .find_entry("anime", &store_id)
        .map_err(|error| store_error("look up ingested entry", error))?
    {
        Some(id) => id,
        None => store
            .add_entry("anime", &store_id, Some(&metadata.title))
            .map_err(|error| store_error("add ingested entry", error))?,
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
    rel: Option<String>,
    slope: Option<f32>,
    /// The filter language in `score_filter`.
    #[serde(default)]
    q: String,
    offset: Option<usize>,
    limit: Option<usize>,
    /// Another run to report rank changes against (`delta`) -- the
    /// client passes the parameters from before the last rerun.
    cmp_rel: Option<String>,
    cmp_slope: Option<f32>,
}

fn rel_param(value: Option<&str>) -> RelScaling {
    match value {
        Some("powq") => RelScaling::Powq,
        _ => RelScaling::Fixed,
    }
}

fn slope_param(value: Option<f32>) -> f32 {
    value.unwrap_or(0.5).clamp(0.0, 1.0)
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

#[derive(Clone, Copy, PartialEq)]
pub(super) struct ScoresKey {
    rel: RelScaling,
    slope_bits: u32,
    generation: u64,
}

/// The last few rankings computed: the current run, the run it's
/// compared against, and a little back-and-forth between them.
#[derive(Default)]
pub(super) struct ScoresCache(std::sync::Mutex<Vec<(ScoresKey, Arc<Vec<Scored>>)>>);

const SCORES_CACHE_SIZE: usize = 4;

/// The full ranking for one parameter set, in rank order. Scoring every
/// entry is the expensive part of the scores view, so rankings are
/// cached until the next store write (`TrackedStore::generation`).
fn scored(
    state: &AppState,
    rel: RelScaling,
    slope: f32,
) -> Result<Arc<Vec<Scored>>, (StatusCode, Json<ApiError>)> {
    let rank = |snapshot: &nrs_core::Snapshot, k_feat| -> Vec<Scored> {
        let hints = KindHints::new(snapshot);
        rows(
            snapshot,
            SortBy::Value,
            SortDir::Desc,
            rel,
            Mode::Credit,
            slope,
            k_feat,
        )
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
    let cached = |key: ScoresKey| {
        let cache = state.scores_cache.0.lock().ok()?;
        cache
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, ranking)| ranking.clone())
    };
    let key = |generation| ScoresKey {
        rel,
        slope_bits: slope.to_bits(),
        generation,
    };

    let (key, ranking) = if let Some(store) = &state.store {
        let store = store
            .lock()
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))?;
        let key = key(store.generation);
        if let Some(hit) = cached(key) {
            return Ok(hit);
        }
        let (snapshot, params) = store
            .load_snapshot()
            .map_err(|error| store_error("load scores", error))?;
        drop(store);
        (key, rank(&snapshot, params.k_feat))
    } else {
        let key = key(0);
        if let Some(hit) = cached(key) {
            return Ok(hit);
        }
        (key, rank(&state.snapshot, nrs_core::KFeatMatrix::default()))
    };

    let ranking = Arc::new(ranking);
    if let Ok(mut cache) = state.scores_cache.0.lock() {
        // Rankings from before a write can never be hit again.
        cache.retain(|(cached, _)| cached.generation == key.generation);
        cache.insert(0, (key, ranking.clone()));
        cache.truncate(SCORES_CACHE_SIZE);
    }
    Ok(ranking)
}

/// A ranking as one request sees it: the run asked for, the run it's
/// compared against (if any), and the parsed filter.
struct ScoresView {
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
        let rel = rel_param(query.rel.as_deref());
        let slope = slope_param(query.slope);
        let ranking = scored(state, rel, slope)?;
        let previous_rank = match (query.cmp_rel.as_deref(), query.cmp_slope) {
            (None, None) => None,
            (cmp_rel, cmp_slope) => {
                let cmp_rel = rel_param(cmp_rel);
                let cmp_slope = slope_param(cmp_slope);
                (cmp_rel != rel || cmp_slope != slope)
                    .then(|| scored(state, cmp_rel, cmp_slope))
                    .transpose()?
                    .map(|previous| {
                        previous
                            .iter()
                            .map(|row| (row.id.clone(), row.rank))
                            .collect()
                    })
            }
        };
        let filter = ScoreFilter::parse(&query.q, previous_rank.is_some());
        Ok(Self {
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
    /// Places gained since the `cmp_*` run (negative: lost). Absent
    /// when nothing is being compared, or for an entry that run lacked.
    delta: Option<i64>,
    /// `rank` and `delta` among this filter's matches only.
    filtered_rank: usize,
    filtered_delta: Option<i64>,
}

#[derive(Serialize)]
pub(super) struct ScoresPage {
    rows: Vec<ScoreRow>,
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

#[derive(Serialize)]
pub(super) struct KfeatResponse {
    pairs: Vec<KfeatPair>,
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
    let anchors = used_anchors(&snapshot);
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
    Ok(Json(KfeatResponse { pairs }))
}

#[derive(Deserialize)]
pub(super) struct KfeatUpdate {
    a: String,
    b: String,
    value: f32,
}

pub(super) async fn kfeat_set(
    State(state): State<Arc<AppState>>,
    Json(update): Json<KfeatUpdate>,
) -> ApiResult<MutationResponse> {
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
            update.value.clamp(0.0, 1.0),
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
    let hints = Arc::new(KindHints::new(&snapshot));
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
    note: Option<String>,
    source_comment: Option<String>,
    needs_review: bool,
    atoms: Vec<Atom>,
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

#[derive(Serialize)]
pub(super) struct EntryResponse {
    entry: EntrySummary,
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
            Some(Relation {
                id: relation.id,
                kind: relation_kind(relation.kind),
                section,
                label,
                from: relation.from.0,
                to: relation.to.0,
                other: EntrySummary::new(other, &hints),
                editable: relation.dah_params.is_some() && relation.kind != RelKind::Credit,
                transform: relation_transform(&relation.transform),
                params: relation
                    .dah_params
                    .as_deref()
                    .and_then(|value| serde_json::from_str(value).ok())
                    .unwrap_or_else(|| json!({})),
            })
        })
        .collect();
    Ok(Json(EntryResponse {
        entry: EntrySummary::new(entry, &hints),
        impacts: impacts
            .iter()
            .map(|impact| impact_response(impact, &lookup, &hints))
            .collect(),
        relations: relation_responses,
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
    let id = ImpactId(impact_id.clone());
    let existing = scoped_impact(&store, &entry_id, &impact_id)?;
    if existing.dah_kind.is_none() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "use the raw editor for an impact without standards metadata",
        ));
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
    scoped_impact(&store, &entry_id, &impact_id)?;
    store
        .delete_impact(&ImpactId(impact_id.clone()))
        .map_err(|error| store_error("delete impact", error))?;
    Ok(Json(MutationResponse {
        ok: true,
        id: Some(impact_id),
    }))
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
    let relation = relation_from_fields(&fields)?;
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
    if existing.dah_params.is_none() || existing.kind == RelKind::Credit {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "this imported relation has no editable standards parameters",
        ));
    }
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

        let compared = page(ScoresQuery {
            cmp_slope: Some(0.2),
            ..Default::default()
        })
        .await
        .ok()
        .unwrap()
        .0;
        assert!(compared.rows.iter().all(|row| row.delta == Some(0)));

        // A filtered view ranks its matches 1..N, keeping the overall
        // rank alongside.
        let filtered = page(ScoresQuery {
            q: "rank>=2".into(),
            cmp_slope: Some(0.2),
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
    async fn imported_relations_cannot_be_rebuilt_with_default_weights() {
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
        let result = relation_edit(
            State(state.clone()),
            Path((a.clone(), id)),
            Json(HashMap::from([
                ("kind".into(), "contains".into()),
                ("from_entry".into(), a.clone()),
                ("to_entry".into(), b),
                ("factor".into(), "1".into()),
            ])),
        )
        .await;
        assert_eq!(result.err().unwrap().0, StatusCode::BAD_REQUEST);
        let store = state.store.as_ref().unwrap().lock().unwrap();
        assert_eq!(
            store.relations_for_entry(&EntryId(a)).unwrap()[0].transform,
            Transform::Scalar(0.37)
        );
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
