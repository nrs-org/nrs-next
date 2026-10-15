//! Watch/read progress (`nrs_store::Progress`): one entry's status and
//! count, and the list of everything tracked.
//!
//! A scored row and its impacts are kept in step both ways. Every write to
//! the row (a step, an edit, a finish) rewrites the consumed impact from
//! the count, `boredom` and `unit_minutes` (`anime_consumed` by episode,
//! else `consumed` by minutes), and keeps a `dropped` impact exactly while
//! the status is dropped (`sync_impacts`). Editing the linked consumed
//! impact in the kind form writes its values back to the row
//! (`impact_edited`); deleting, raw-editing or sharing it turns syncing
//! off (`impact_detached`), so a hand change isn't overwritten.

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
    boredom: Option<f32>,
    unit_minutes: Option<f32>,
    scored: bool,
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
            boredom: progress.boredom,
            unit_minutes: progress.unit_minutes,
            scored: progress.scored,
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

/// Every progress write ends here, so it's also where a written row is
/// queued for MyAnimeList/AniList (`lists::live`; dropped when neither
/// is connected).
fn write(store: &mut TrackedStore, progress: &Progress) -> Result<(), ApiErr> {
    store.set_progress(progress, None).map_err(invalid)?;
    if let Ok(entries) = store.list_entries() {
        if let Some(entry) = entries.iter().find(|entry| entry.id == progress.entry) {
            crate::lists::live::enqueue(crate::lists::plan::row(entry, progress.clone()));
        }
    }
    Ok(())
}

/// Validates, brings the impacts in line, then saves the row (whose links
/// the sync may have changed). Validating first means a rejected row
/// leaves no impacts behind.
fn save(store: &mut TrackedStore, progress: &mut Progress) -> Result<(), ApiErr> {
    progress.validate().map_err(invalid)?;
    sync_impacts(store, progress)?;
    write(store, progress)
}

pub(crate) async fn progress_get(
    State(state): State<Arc<AppState>>,
    Path(entry_id): Path<String>,
) -> ApiResult<ProgressResponse> {
    let store = lock_store(&state)?;
    respond(load(&store, &EntryId(entry_id))?.as_ref())
}

fn yes() -> bool {
    true
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
    boredom: Option<f32>,
    /// Ignored for `minute`, which is 1.
    unit_minutes: Option<f32>,
    #[serde(default = "yes")]
    scored: bool,
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
        boredom: input.boredom,
        unit_minutes: match input.unit {
            ProgressUnit::Minute => Some(1.0),
            _ => input.unit_minutes,
        },
        scored: input.scored,
    };
    save(&mut store, &mut progress)?;
    respond(Some(&progress))
}

/// The consumed impact `progress` should have, as kind form fields:
/// `None` with nothing consumed yet, or minutes per unit unknown.
fn consumed_fields(progress: &Progress) -> Option<HashMap<String, String>> {
    let minutes = progress.unit_minutes?;
    if progress.current <= 0.0 {
        return None;
    }
    let boredom = progress.boredom.unwrap_or(1.0);
    let mut fields = HashMap::from([("boredom".to_string(), boredom.to_string())]);
    if progress.unit == ProgressUnit::Episode {
        fields.insert("kind".into(), "anime_consumed".into());
        fields.insert("episodes".into(), progress.current.to_string());
        fields.insert("episode_duration_minutes".into(), minutes.to_string());
    } else {
        fields.insert("kind".into(), "consumed".into());
        fields.insert(
            "duration_minutes".into(),
            (progress.current * minutes).to_string(),
        );
    }
    Some(fields)
}

/// Makes the linked impacts match `progress` (see the module docs).
/// Nothing is touched while it isn't `scored`.
fn sync_impacts(store: &mut TrackedStore, progress: &mut Progress) -> Result<(), ApiErr> {
    if !progress.scored {
        return Ok(());
    }
    let entry_id = progress.entry.0.clone();
    match (consumed_fields(progress), progress.consumed_impact.clone()) {
        (Some(fields), existing) => {
            progress.consumed_impact =
                Some(upsert_impact(store, &entry_id, existing.as_ref(), fields)?);
        }
        // Back to nothing consumed. (Unknown minutes per unit keep what's
        // there: the count alone can't say what it's worth.)
        (None, Some(id)) if progress.current <= 0.0 => {
            store
                .delete_impact(&id)
                .map_err(|error| store_error("delete consumed impact", error))?;
            progress.consumed_impact = None;
        }
        (None, _) => {}
    }
    let dropped = progress.status == ProgressStatus::Dropped;
    match (dropped, progress.dropped_impact.clone()) {
        (true, None) => {
            let fields = HashMap::from([("kind".to_string(), "dropped".to_string())]);
            progress.dropped_impact = Some(upsert_impact(store, &entry_id, None, fields)?);
        }
        // Picked back up: the drop no longer holds.
        (false, Some(id)) => {
            store
                .delete_impact(&id)
                .map_err(|error| store_error("delete dropped impact", error))?;
            progress.dropped_impact = None;
        }
        _ => {}
    }
    Ok(())
}

/// A stored kind parameter as a number (they're kept as form strings).
fn param(params: &serde_json::Value, key: &str) -> Option<f32> {
    match &params[key] {
        serde_json::Value::String(value) => value.trim().parse().ok(),
        value => value.as_f64().map(|value| value as f32),
    }
}

/// Whether `impact` already says what `fields` would write, so a status
/// change or a step that doesn't move the count adds no impact event.
fn same_values(impact: &nrs_store::ReviewImpact, fields: &HashMap<String, String>) -> bool {
    if impact.dah_kind.as_deref() != fields.get("kind").map(String::as_str) {
        return false;
    }
    let Some(stored) = impact
        .dah_params
        .as_deref()
        .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
    else {
        return false;
    };
    fields
        .iter()
        .filter(|(key, _)| *key != "kind")
        .all(|(key, value)| {
            let wanted = value.parse::<f32>().ok();
            match (param(&stored, key), wanted) {
                (Some(stored), Some(wanted)) => (stored - wanted).abs() < 1e-4,
                _ => false,
            }
        })
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
        if same_values(&impact, &fields) {
            return Ok(id.clone());
        }
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

/// The scored progress row whose impacts include `impact_id`, if any.
/// Look it up before deleting the impact: afterwards the link reads as
/// `None`.
pub(super) fn linked_row(
    store: &TrackedStore,
    entry_id: &str,
    impact_id: &str,
) -> Result<Option<Progress>, ApiErr> {
    Ok(load(store, &EntryId(entry_id.into()))?.filter(|progress| {
        progress.scored
            && [&progress.consumed_impact, &progress.dropped_impact]
                .into_iter()
                .flatten()
                .any(|id| id.0 == impact_id)
    }))
}

/// A linked impact was changed in a way the row can't follow (deleted,
/// raw-edited, shared): stop syncing, so the next step doesn't undo it.
pub(super) fn impact_detached(
    store: &mut TrackedStore,
    row: Option<Progress>,
) -> Result<(), ApiErr> {
    let Some(mut progress) = row else {
        return Ok(());
    };
    progress.scored = false;
    write(store, &progress)
}

/// The linked impact `impact_id` was just saved through the kind form:
/// carry its values back to the row, or stop syncing if they no longer
/// describe it (changed to another kind). Edits to the linked dropped
/// impact are left alone.
pub(super) fn impact_edited(
    store: &mut TrackedStore,
    entry_id: &str,
    impact_id: &str,
) -> Result<(), ApiErr> {
    let Some(mut progress) = linked_row(store, entry_id, impact_id)? else {
        return Ok(());
    };
    if progress.consumed_impact.as_ref().map(|id| id.0.as_str()) != Some(impact_id) {
        return Ok(());
    }
    let impact = scoped_impact(store, entry_id, impact_id)?;
    let params: serde_json::Value = impact
        .dah_params
        .as_deref()
        .and_then(|params| serde_json::from_str(params).ok())
        .unwrap_or_default();
    let kind = impact.dah_kind.as_deref();
    let follows = match (kind, progress.unit) {
        (Some("anime_consumed"), ProgressUnit::Episode) => {
            match (
                param(&params, "episodes"),
                param(&params, "episode_duration_minutes"),
            ) {
                (Some(episodes), Some(minutes)) if minutes > 0.0 => {
                    progress.current = episodes;
                    progress.unit_minutes = Some(minutes);
                    true
                }
                _ => false,
            }
        }
        (Some("consumed"), unit) if unit != ProgressUnit::Episode => {
            match (param(&params, "duration_minutes"), progress.unit_minutes) {
                (Some(duration), Some(minutes)) => {
                    progress.current = duration / minutes;
                    true
                }
                _ => false,
            }
        }
        _ => false,
    };
    if follows {
        progress.boredom = param(&params, "boredom").or(progress.boredom);
    } else {
        progress.scored = false;
    }
    write(store, &progress)
}

#[derive(Deserialize)]
pub(crate) struct StepInput {
    by: f32,
}

/// Moves the count by `by` (clamped to `[0, total]`; a total of 0 is
/// unknown, like the stores' count for a show still airing). Stepping
/// forward starts a planned or paused entry and dates its start; reaching
/// the total is left for the client to confirm as a finish.
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
    if let Some(total) = progress.total.filter(|&total| total > 0.0) {
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
    save(&mut store, &mut progress)?;
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

    fn row(unit: ProgressUnit, current: f32, unit_minutes: Option<f32>) -> Progress {
        let mut progress = Progress::new(EntryId("e".into()), ProgressStatus::Active, unit);
        progress.current = current;
        progress.unit_minutes = unit_minutes;
        progress
    }

    #[test]
    fn consumed_fields_follow_the_unit() {
        let episodes = consumed_fields(&row(ProgressUnit::Episode, 3.0, Some(20.0))).unwrap();
        assert_eq!(episodes["kind"], "anime_consumed");
        assert_eq!(
            (episodes["episodes"].as_str(), episodes["boredom"].as_str()),
            ("3", "1")
        );
        let chapters = consumed_fields(&row(ProgressUnit::Chapter, 10.0, Some(6.0))).unwrap();
        assert_eq!(
            (
                chapters["kind"].as_str(),
                chapters["duration_minutes"].as_str()
            ),
            ("consumed", "60")
        );
        // Nothing watched, or no idea what a chapter is worth: no impact.
        assert!(consumed_fields(&row(ProgressUnit::Episode, 0.0, Some(20.0))).is_none());
        assert!(consumed_fields(&row(ProgressUnit::Chapter, 10.0, None)).is_none());
    }

    #[test]
    fn same_values_compares_stored_numbers_not_strings() {
        let fields = consumed_fields(&row(ProgressUnit::Episode, 3.0, Some(20.0))).unwrap();
        let mut impact = nrs_store::ReviewImpact {
            id: ImpactId("i".into()),
            target: EntryId("e".into()),
            atoms: Vec::new(),
            note: None,
            source_comment: None,
            needs_review: false,
            dah_kind: Some("anime_consumed".into()),
            dah_params: Some(
                r#"{"boredom":"1.0","episodes":"3","episode_duration_minutes":"20","source":"e"}"#
                    .into(),
            ),
            dah_factor: Some(1.0),
            members: Vec::new(),
            split: None,
            split_stored: false,
        };
        assert!(same_values(&impact, &fields));
        impact.dah_params =
            Some(r#"{"boredom":"1","episodes":"2","episode_duration_minutes":"20"}"#.into());
        assert!(!same_values(&impact, &fields));
    }

    #[test]
    fn days_round_trip_through_iso_dates() {
        assert_eq!(parse_day("2021-06-02").unwrap(), 18780);
        assert_eq!(format_day(18780), "2021-06-02");
        assert!(parse_day("06/02/2021").is_err());
    }
}
