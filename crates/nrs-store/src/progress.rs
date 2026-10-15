//! Where an entry stands for its viewer: planned, being watched/read,
//! finished or dropped, and how far along (the `progress` table, V11).
//!
//! Progress is tracking, not scoring: nothing here produces atoms. The
//! consumed/dropped impacts a finished or dropped entry is scored by are
//! ordinary impacts the app writes and links back here
//! (`consumed_impact`, `dropped_impact`), so it can update them instead
//! of adding duplicates. A link to an impact that no longer exists
//! (deleted by hand, removed by a resync) reads back as `None`.

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use nrs_core::{EntryId, ImpactId};

use crate::crud::{append_event, to_json};
use crate::error::{Result, StoreError};
use crate::event::EventKind;
use crate::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressStatus {
    Planned,
    Active,
    Paused,
    Completed,
    Dropped,
}

impl ProgressStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProgressStatus::Planned => "planned",
            ProgressStatus::Active => "active",
            ProgressStatus::Paused => "paused",
            ProgressStatus::Completed => "completed",
            ProgressStatus::Dropped => "dropped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "planned" => ProgressStatus::Planned,
            "active" => ProgressStatus::Active,
            "paused" => ProgressStatus::Paused,
            "completed" => ProgressStatus::Completed,
            "dropped" => ProgressStatus::Dropped,
            _ => return None,
        })
    }
}

/// What `current`/`total` count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressUnit {
    Episode,
    Chapter,
    Volume,
    Minute,
}

impl ProgressUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            ProgressUnit::Episode => "episode",
            ProgressUnit::Chapter => "chapter",
            ProgressUnit::Volume => "volume",
            ProgressUnit::Minute => "minute",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "episode" => ProgressUnit::Episode,
            "chapter" => ProgressUnit::Chapter,
            "volume" => ProgressUnit::Volume,
            "minute" => ProgressUnit::Minute,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub entry: EntryId,
    pub status: ProgressStatus,
    pub unit: ProgressUnit,
    pub current: f32,
    /// `None` when unknown (an airing show, a dropped entry imported
    /// without its store count).
    pub total: Option<f32>,
    /// Days since 1970-01-01, like `TimeSupport::Interval`.
    pub started: Option<i32>,
    pub finished: Option<i32>,
    /// Finished viewings before the current one.
    pub rewatch: u32,
    pub consumed_impact: Option<ImpactId>,
    pub dropped_impact: Option<ImpactId>,
}

impl Progress {
    pub fn new(entry: EntryId, status: ProgressStatus, unit: ProgressUnit) -> Self {
        Self {
            entry,
            status,
            unit,
            current: 0.0,
            total: None,
            started: None,
            finished: None,
            rewatch: 0,
            consumed_impact: None,
            dropped_impact: None,
        }
    }

    /// What `set_progress` checks before writing.
    pub fn validate(&self) -> Result<()> {
        let invalid = |message: &str| Err(StoreError::Invalid(message.into()));
        if !self.current.is_finite() || self.current < 0.0 {
            return invalid("progress must be a non-negative number");
        }
        if self
            .total
            .is_some_and(|total| !total.is_finite() || total < 0.0)
        {
            return invalid("total must be a non-negative number");
        }
        if let (Some(started), Some(finished)) = (self.started, self.finished) {
            if finished < started {
                return invalid("finished before it started");
            }
        }
        Ok(())
    }
}

/// Linked impacts are joined in so a dangling link reads as `None`.
const SELECT: &str = "SELECT p.entry, p.status, p.unit, p.current, p.total, p.started, \
                             p.finished, p.rewatch, ci.id, di.id \
                        FROM progress p \
                        LEFT JOIN impact ci ON ci.id = p.consumed_impact \
                        LEFT JOIN impact di ON di.id = p.dropped_impact";

fn read_row(row: &Row) -> rusqlite::Result<(Progress, String, String)> {
    let status: String = row.get(1)?;
    let unit: String = row.get(2)?;
    let progress = Progress {
        entry: EntryId(row.get(0)?),
        // Placeholders until `parse_row` checks the stored text.
        status: ProgressStatus::Planned,
        unit: ProgressUnit::Episode,
        current: row.get(3)?,
        total: row.get(4)?,
        started: row.get(5)?,
        finished: row.get(6)?,
        rewatch: row.get(7)?,
        consumed_impact: row.get::<_, Option<String>>(8)?.map(ImpactId),
        dropped_impact: row.get::<_, Option<String>>(9)?.map(ImpactId),
    };
    Ok((progress, status, unit))
}

fn parse_row((mut progress, status, unit): (Progress, String, String)) -> Result<Progress> {
    progress.status = ProgressStatus::parse(&status)
        .ok_or_else(|| StoreError::InvalidData(format!("progress status {status:?}")))?;
    progress.unit = ProgressUnit::parse(&unit)
        .ok_or_else(|| StoreError::InvalidData(format!("progress unit {unit:?}")))?;
    Ok(progress)
}

impl Store {
    pub fn progress(&self, entry: &EntryId) -> Result<Option<Progress>> {
        self.conn
            .query_row(
                &format!("{SELECT} WHERE p.entry = ?1"),
                params![entry.0],
                read_row,
            )
            .optional()?
            .map(parse_row)
            .transpose()
    }

    pub fn list_progress(&self) -> Result<Vec<Progress>> {
        let mut stmt = self.conn.prepare(&format!("{SELECT} ORDER BY p.rowid"))?;
        let rows = stmt.query_map([], read_row)?;
        rows.map(|row| parse_row(row?)).collect()
    }

    /// Creates or replaces `progress.entry`'s row. A write that changes
    /// nothing appends no event.
    pub fn set_progress(&mut self, progress: &Progress, note: Option<&str>) -> Result<()> {
        progress.validate()?;
        let before = self.progress(&progress.entry)?;
        if before.as_ref() == Some(progress) {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        let exists = tx
            .query_row(
                "SELECT 1 FROM entry WHERE id = ?1",
                params![progress.entry.0],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(StoreError::Invalid(format!(
                "no entry {}",
                progress.entry.0
            )));
        }
        tx.execute(
            "INSERT OR REPLACE INTO progress \
               (entry, status, unit, current, total, started, finished, rewatch, \
                consumed_impact, dropped_impact) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                progress.entry.0,
                progress.status.as_str(),
                progress.unit.as_str(),
                progress.current,
                progress.total,
                progress.started,
                progress.finished,
                progress.rewatch,
                progress.consumed_impact.as_ref().map(|id| &id.0),
                progress.dropped_impact.as_ref().map(|id| &id.0),
            ],
        )?;
        append_event(
            &tx,
            EventKind::ProgressSet,
            Some(&progress.entry.0),
            to_json(before.as_ref())?.as_deref(),
            to_json(Some(progress))?.as_deref(),
            note,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Stops tracking `entry`. Its linked impacts are kept.
    pub fn clear_progress(&mut self, entry: &EntryId) -> Result<()> {
        let Some(before) = self.progress(entry)? else {
            return Ok(());
        };
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM progress WHERE entry = ?1", params![entry.0])?;
        append_event(
            &tx,
            EventKind::ProgressSet,
            Some(&entry.0),
            to_json(Some(&before))?.as_deref(),
            None,
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Progress rows implied by the consumed impacts of entries that have
    /// none yet: `anime_consumed` counts episodes, `consumed` minutes; a
    /// `dropped` impact beside one means dropped, otherwise completed.
    /// Shared impacts are left alone (one consumption split between
    /// entries says nothing about any one of them).
    pub fn plan_progress_backfill(&self) -> Result<Vec<Progress>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.target, i.dah_kind, i.dah_params \
               FROM impact i \
              WHERE i.dah_kind IN ('anime_consumed', 'consumed', 'dropped') \
                AND NOT EXISTS (SELECT 1 FROM impact_member m WHERE m.impact = i.id) \
                AND NOT EXISTS (SELECT 1 FROM progress p WHERE p.entry = i.target) \
              ORDER BY i.rowid",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut planned: Vec<Progress> = Vec::new();
        let mut dropped = Vec::new();
        for row in rows {
            let (id, target, kind, params) = row?;
            if kind == "dropped" {
                dropped.push((target, id));
                continue;
            }
            // One consumption per entry: a second consumed impact is a
            // rewatch the row can't describe, so the first one wins.
            if planned.iter().any(|progress| progress.entry.0 == target) {
                continue;
            }
            let params: serde_json::Value = match params {
                Some(params) => serde_json::from_str(&params)?,
                None => serde_json::Value::Null,
            };
            let number = |key: &str| {
                params[key]
                    .as_str()
                    .and_then(|value| value.parse::<f32>().ok())
                    .or_else(|| params[key].as_f64().map(|value| value as f32))
            };
            let (unit, current) = if kind == "anime_consumed" {
                (ProgressUnit::Episode, number("episodes"))
            } else {
                (ProgressUnit::Minute, number("duration_minutes"))
            };
            let Some(current) = current else {
                continue;
            };
            let mut progress = Progress::new(EntryId(target), ProgressStatus::Completed, unit);
            progress.current = current;
            progress.total = Some(current);
            progress.consumed_impact = Some(ImpactId(id));
            planned.push(progress);
        }
        for (target, id) in dropped {
            if let Some(progress) = planned
                .iter_mut()
                .find(|progress| progress.entry.0 == target)
            {
                progress.status = ProgressStatus::Dropped;
                // How far the whole thing runs isn't recorded for a drop.
                progress.total = None;
                progress.dropped_impact = Some(ImpactId(id));
            }
        }
        Ok(planned)
    }
}

#[cfg(test)]
mod tests {
    use nrs_core::{AnchorId, Atom, SourceId, TimeSupport};

    use super::*;

    fn atom(source: &str) -> Atom {
        Atom {
            source: SourceId(source.into()),
            blend: vec![(AnchorId("boredom".into()), 1.0)],
            magnitude: 0.3,
            time: TimeSupport::Moment,
        }
    }

    fn impact(store: &mut Store, entry: &EntryId, kind: &str, params: &str) -> ImpactId {
        let id = store.add_impact(entry, &[atom(&entry.0)], None).unwrap();
        store.set_impact_dah(&id, kind, params, 1.0).unwrap();
        id
    }

    #[test]
    fn set_progress_round_trips_and_logs_each_change() {
        let mut store = Store::open_in_memory().unwrap();
        let entry = store
            .add_entry("url", "https://myanimelist.net/anime/1", None)
            .unwrap();
        let mut progress =
            Progress::new(entry.clone(), ProgressStatus::Active, ProgressUnit::Episode);
        progress.current = 3.0;
        progress.total = Some(12.0);
        progress.started = Some(20_000);
        store.set_progress(&progress, None).unwrap();
        assert_eq!(store.progress(&entry).unwrap(), Some(progress.clone()));

        // Unchanged: no second event.
        store.set_progress(&progress, None).unwrap();
        progress.current = 4.0;
        store.set_progress(&progress, None).unwrap();
        let events: Vec<_> = store
            .event_log()
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "progress.set")
            .collect();
        assert_eq!(events.len(), 2);
        assert!(events[1]
            .before
            .as_deref()
            .unwrap()
            .contains("\"current\":3.0"));

        store.clear_progress(&entry).unwrap();
        assert_eq!(store.progress(&entry).unwrap(), None);
        assert_eq!(store.list_progress().unwrap(), Vec::new());
    }

    #[test]
    fn set_progress_rejects_bad_values_and_unknown_entries() {
        let mut store = Store::open_in_memory().unwrap();
        let entry = store.add_entry("url", "u", None).unwrap();
        let mut progress = Progress::new(entry, ProgressStatus::Active, ProgressUnit::Chapter);
        progress.current = -1.0;
        assert!(store.set_progress(&progress, None).is_err());
        progress.current = 1.0;
        progress.started = Some(10);
        progress.finished = Some(9);
        assert!(store.set_progress(&progress, None).is_err());
        let ghost = Progress::new(
            EntryId("nope".into()),
            ProgressStatus::Planned,
            ProgressUnit::Episode,
        );
        assert!(store.set_progress(&ghost, None).is_err());
    }

    #[test]
    fn a_deleted_linked_impact_reads_back_unlinked() {
        let mut store = Store::open_in_memory().unwrap();
        let entry = store.add_entry("url", "u", None).unwrap();
        let consumed = impact(
            &mut store,
            &entry,
            "consumed",
            r#"{"duration_minutes":"60"}"#,
        );
        let mut progress = Progress::new(
            entry.clone(),
            ProgressStatus::Completed,
            ProgressUnit::Minute,
        );
        progress.consumed_impact = Some(consumed.clone());
        store.set_progress(&progress, None).unwrap();
        store.delete_impact(&consumed).unwrap();
        assert_eq!(
            store.progress(&entry).unwrap().unwrap().consumed_impact,
            None
        );
    }

    #[test]
    fn backfill_reads_consumed_and_dropped_impacts() {
        let mut store = Store::open_in_memory().unwrap();
        let watched = store.add_entry("url", "a", None).unwrap();
        let dropped = store.add_entry("url", "b", None).unwrap();
        let played = store.add_entry("url", "c", None).unwrap();
        let tracked = store.add_entry("url", "d", None).unwrap();
        let watched_impact = impact(
            &mut store,
            &watched,
            "anime_consumed",
            r#"{"boredom":"1","episodes":"12","episode_duration_minutes":"20"}"#,
        );
        impact(
            &mut store,
            &dropped,
            "anime_consumed",
            r#"{"boredom":"0.7","episodes":"3"}"#,
        );
        let drop_impact = impact(&mut store, &dropped, "dropped", "{}");
        impact(
            &mut store,
            &played,
            "consumed",
            r#"{"boredom":"1","duration_minutes":"600"}"#,
        );
        impact(
            &mut store,
            &tracked,
            "anime_consumed",
            r#"{"episodes":"5"}"#,
        );
        store
            .set_progress(
                &Progress::new(tracked, ProgressStatus::Active, ProgressUnit::Episode),
                None,
            )
            .unwrap();

        let plan = store.plan_progress_backfill().unwrap();
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].entry, watched);
        assert_eq!(plan[0].status, ProgressStatus::Completed);
        assert_eq!((plan[0].current, plan[0].total), (12.0, Some(12.0)));
        assert_eq!(plan[0].consumed_impact, Some(watched_impact));
        assert_eq!(plan[1].entry, dropped);
        assert_eq!(plan[1].status, ProgressStatus::Dropped);
        assert_eq!((plan[1].current, plan[1].total), (3.0, None));
        assert_eq!(plan[1].dropped_impact, Some(drop_impact));
        assert_eq!(plan[2].entry, played);
        assert_eq!(
            (plan[2].unit, plan[2].current),
            (ProgressUnit::Minute, 600.0)
        );
    }
}
