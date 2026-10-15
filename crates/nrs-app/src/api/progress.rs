//! Watch/read progress (`nrs_store::Progress`): one entry's status and
//! count, and the list of everything tracked.
//!
//! Finishing or dropping an entry can also score it: a `PUT` with
//! `score` writes the consumed impact from the count (`anime_consumed`
//! by episode, else `consumed` by minutes) and, for a drop, a `dropped`
//! impact, and links both to the row so the next finish updates them
//! rather than adding more. Without `score` no impact is touched.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use nrs_core::{EntryId, ImpactId};
use nrs_store::{Progress, ProgressStatus, ProgressUnit};
use serde::{Deserialize, Serialize};

use super::{
    api_error, dah_params_json, kind_hints, prepare_impact, scoped_impact, store_error, ApiError,
    ApiResult, EntrySummary, MutationResponse,
};
use crate::{AppState, TrackedStore};

type ApiErr = (StatusCode, Json<ApiError>);

const EPOCH: time::Date = time::macros::date!(1970 - 01 - 01);

fn day_number(date: time::Date) -> i32 {
    date.to_julian_day() - EPOCH.to_julian_day()
}

fn parse_day(value: &str) -> Result<i32, ApiErr> {
    let format = time::format_description::well_known::Iso8601::DATE;
    time::Date::parse(value.trim(), &format)
        .map(day_number)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, format!("bad date {value:?}")))
}

fn format_day(day: i32) -> String {
    time::Date::from_julian_day(EPOCH.to_julian_day() + day)
        .map(|date| date.to_string())
        .unwrap_or_default()
}

fn today() -> i32 {
    day_number(time::OffsetDateTime::now_utc().date())
}

#[derive(Serialize)]
pub(crate) struct ProgressView {
    status: ProgressStatus,
    unit: ProgressUnit,
    current: f32,
    total: Option<f32>,
    /// `YYYY-MM-DD`.
    started: Option<String>,
    finished: Option<String>,
    rewatch: u32,
    consumed_impact: Option<String>,
    dropped_impact: Option<String>,
}

impl From<&Progress> for ProgressView {
    fn from(progress: &Progress) -> Self {
        Self {
            status: progress.status,
            unit: progress.unit,
            current: progress.current,
            total: progress.total,
            started: progress.started.map(format_day),
            finished: progress.finished.map(format_day),
            rewatch: progress.rewatch,
            consumed_impact: progress.consumed_impact.as_ref().map(|id| id.0.clone()),
            dropped_impact: progress.dropped_impact.as_ref().map(|id| id.0.clone()),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ProgressResponse {
    progress: Option<ProgressView>,
}

fn lock_store(state: &AppState) -> Result<std::sync::MutexGuard<'_, TrackedStore>, ApiErr> {
    state
        .store
        .as_ref()
        .ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "progress requires database mode",
            )
        })?
        .lock()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "store lock poisoned"))
}

fn load(store: &TrackedStore, entry: &EntryId) -> Result<Option<Progress>, ApiErr> {
    store
        .progress(entry)
        .map_err(|error| store_error("load progress", error))
}

fn respond(progress: Option<&Progress>) -> ApiResult<ProgressResponse> {
    Ok(Json(ProgressResponse {
        progress: progress.map(ProgressView::from),
    }))
}

fn invalid(error: nrs_store::StoreError) -> ApiErr {
    match error {
        nrs_store::StoreError::Invalid(message) => api_error(StatusCode::BAD_REQUEST, message),
        error => store_error("save progress", error),
    }
}

fn write(store: &mut TrackedStore, progress: &Progress) -> Result<(), ApiErr> {
    store.set_progress(progress, None).map_err(invalid)
}

pub(crate) async fn progress_get(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
) -> ApiResult<ProgressResponse> {
    let store = lock_store(&state)?;
    respond(load(&store, &EntryId(entry_id))?.as_ref())
}

/// What to score a finish or a drop with.
#[derive(Deserialize)]
pub(crate) struct ScoreInput {
    boredom: f32,
    /// Minutes per counted unit; ignored for `minute`.
    unit_minutes: Option<f32>,
}

#[derive(Deserialize)]
pub(crate) struct ProgressInput {
    status: ProgressStatus,
    unit: ProgressUnit,
    current: f32,
    total: Option<f32>,
    started: Option<String>,
    finished: Option<String>,
    #[serde(default)]
    rewatch: u32,
    score: Option<ScoreInput>,
}

fn optional_day(value: Option<&str>) -> Result<Option<i32>, ApiErr> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_day)
        .transpose()
}

pub(crate) async fn progress_set(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
    Json(input): Json<ProgressInput>,
) -> ApiResult<ProgressResponse> {
    let entry = EntryId(entry_id);
    let mut store = lock_store(&state)?;
    let before = load(&store, &entry)?;
    let mut progress = Progress {
        entry: entry.clone(),
        status: input.status,
        unit: input.unit,
        current: input.current,
        total: input.total,
        started: optional_day(input.started.as_deref())?,
        finished: optional_day(input.finished.as_deref())?,
        rewatch: input.rewatch,
        consumed_impact: before.as_ref().and_then(|p| p.consumed_impact.clone()),
        dropped_impact: before.as_ref().and_then(|p| p.dropped_impact.clone()),
    };
    // Checked before scoring, so a rejected row leaves no impacts behind.
    progress.validate().map_err(invalid)?;
    if let Some(score) = &input.score {
        score_progress(&mut store, &mut progress, score)?;
    }
    write(&mut store, &progress)?;
    respond(Some(&progress))
}

/// Writes (or rewrites) the impacts `progress` is scored by; see the
/// module docs.
fn score_progress(
    store: &mut TrackedStore,
    progress: &mut Progress,
    score: &ScoreInput,
) -> Result<(), ApiErr> {
    let bad = |message: &str| Err(api_error(StatusCode::BAD_REQUEST, message));
    if !matches!(
        progress.status,
        ProgressStatus::Completed | ProgressStatus::Dropped
    ) {
        return bad("only a completed or dropped entry is scored");
    }
    if !(0.0..=1.0).contains(&score.boredom) {
        return bad("boredom must be between 0 and 1");
    }
    let unit_minutes = match progress.unit {
        ProgressUnit::Minute => 1.0,
        _ => match score.unit_minutes {
            Some(minutes) if minutes.is_finite() && minutes > 0.0 => minutes,
            _ => return bad("minutes per unit are needed to score this progress"),
        },
    };
    let entry_id = progress.entry.0.clone();
    if progress.current > 0.0 {
        let mut fields = HashMap::from([("boredom".to_string(), score.boredom.to_string())]);
        if progress.unit == ProgressUnit::Episode {
            fields.insert("kind".into(), "anime_consumed".into());
            fields.insert("episodes".into(), progress.current.to_string());
            fields.insert("episode_duration_minutes".into(), unit_minutes.to_string());
        } else {
            fields.insert("kind".into(), "consumed".into());
            fields.insert(
                "duration_minutes".into(),
                (progress.current * unit_minutes).to_string(),
            );
        }
        progress.consumed_impact = Some(upsert_impact(
            store,
            &entry_id,
            progress.consumed_impact.as_ref(),
            fields,
        )?);
    }
    match (progress.status, progress.dropped_impact.clone()) {
        (ProgressStatus::Dropped, None) => {
            let fields = HashMap::from([("kind".to_string(), "dropped".to_string())]);
            progress.dropped_impact = Some(upsert_impact(store, &entry_id, None, fields)?);
        }
        // Picked back up and finished: the drop no longer holds.
        (ProgressStatus::Completed, Some(id)) => {
            store
                .delete_impact(&id)
                .map_err(|error| store_error("delete dropped impact", error))?;
            progress.dropped_impact = None;
        }
        _ => {}
    }
    Ok(())
}

/// Rewrites `existing` (keeping its note) or adds a new impact from the
/// kind form `fields`.
fn upsert_impact(
    store: &mut TrackedStore,
    entry_id: &str,
    existing: Option<&ImpactId>,
    mut fields: HashMap<String, String>,
) -> Result<ImpactId, ApiErr> {
    if let Some(id) = existing {
        let impact = scoped_impact(store, entry_id, &id.0)?;
        if !impact.members.is_empty() {
            return Err(api_error(
                StatusCode::CONFLICT,
                "the linked consumed impact is shared; edit it on the entry page",
            ));
        }
        if let Some(note) = impact.note {
            fields.insert("note".into(), note);
        }
        let (kind, atoms, note, factor, fields) = prepare_impact(entry_id, fields)?;
        store
            .edit_impact(id, &atoms, note.as_deref())
            .map_err(|error| store_error("edit impact", error))?;
        store
            .set_impact_dah(id, &kind, &dah_params_json(&fields), factor)
            .map_err(|error| store_error("save impact metadata", error))?;
        return Ok(id.clone());
    }
    let (kind, atoms, note, factor, fields) = prepare_impact(entry_id, fields)?;
    let id = store
        .add_impact(&EntryId(entry_id.into()), &atoms, note.as_deref())
        .map_err(|error| store_error("add impact", error))?;
    if let Err(error) = store.set_impact_dah(&id, &kind, &dah_params_json(&fields), factor) {
        let _ = store.delete_impact(&id);
        return Err(store_error("save impact metadata", error));
    }
    Ok(id)
}

#[derive(Deserialize)]
pub(crate) struct StepInput {
    by: f32,
}

/// Moves the count by `by` (clamped to `[0, total]`). Stepping forward
/// starts a planned or paused entry and dates its start; reaching the
/// total is left for the client to confirm as a finish.
pub(crate) async fn progress_step(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
    Json(input): Json<StepInput>,
) -> ApiResult<ProgressResponse> {
    let entry = EntryId(entry_id);
    let mut store = lock_store(&state)?;
    let mut progress = load(&store, &entry)?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "this entry isn't tracked"))?;
    if !input.by.is_finite() {
        return Err(api_error(StatusCode::BAD_REQUEST, "bad step"));
    }
    let mut current = (progress.current + input.by).max(0.0);
    if let Some(total) = progress.total {
        current = current.min(total);
    }
    progress.current = current;
    if input.by > 0.0 {
        if matches!(
            progress.status,
            ProgressStatus::Planned | ProgressStatus::Paused
        ) {
            progress.status = ProgressStatus::Active;
        }
        progress.started.get_or_insert_with(today);
    }
    write(&mut store, &progress)?;
    respond(Some(&progress))
}

pub(crate) async fn progress_clear(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
) -> ApiResult<MutationResponse> {
    let mut store = lock_store(&state)?;
    store
        .clear_progress(&EntryId(entry_id))
        .map_err(|error| store_error("clear progress", error))?;
    Ok(Json(MutationResponse { ok: true, id: None }))
}

#[derive(Deserialize)]
pub(crate) struct ProgressListQuery {
    /// Comma-separated statuses; all when absent.
    status: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ProgressListItem {
    entry: EntrySummary,
    progress: ProgressView,
}

#[derive(Serialize)]
pub(crate) struct ProgressListResponse {
    items: Vec<ProgressListItem>,
}

pub(crate) async fn progress_list(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ProgressListQuery>,
) -> ApiResult<ProgressListResponse> {
    let statuses = match query.status.as_deref().filter(|value| !value.is_empty()) {
        None => None,
        Some(value) => Some(
            value
                .split(',')
                .map(|status| {
                    ProgressStatus::parse(status.trim()).ok_or_else(|| {
                        api_error(
                            StatusCode::BAD_REQUEST,
                            format!("unknown status {status:?}"),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    let store = lock_store(&state)?;
    let rows = store
        .list_progress()
        .map_err(|error| store_error("load progress", error))?;
    let entries = store
        .list_entries()
        .map_err(|error| store_error("load entries", error))?;
    let hints = kind_hints(&state, &store)?;
    drop(store);
    let entries: HashMap<&str, &nrs_core::Entry> = entries
        .iter()
        .map(|entry| (entry.id.0.as_str(), entry))
        .collect();
    let items = rows
        .iter()
        .filter(|progress| {
            statuses
                .as_ref()
                .is_none_or(|statuses| statuses.contains(&progress.status))
        })
        .filter_map(|progress| {
            let entry = entries.get(progress.entry.0.as_str())?;
            Some(ProgressListItem {
                entry: EntrySummary::new(entry, &hints),
                progress: progress.into(),
            })
        })
        .collect();
    Ok(Json(ProgressListResponse { items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_round_trip_through_iso_dates() {
        assert_eq!(parse_day("2021-06-02").unwrap(), 18780);
        assert_eq!(format_day(18780), "2021-06-02");
        assert!(parse_day("06/02/2021").is_err());
    }
}
