//! Pairwise "A beats B" corrections (INFRA.md §8's fitting fuel for
//! `nrs-fit`), persisted purely as events (INFRA.md §6: "the corrections
//! and brackets you record are events `nrs-fit` reads") -- no separate
//! table.

use serde::{Deserialize, Serialize};

use nrs_core::EntryId;

use crate::crud::{append_event, to_json};
use crate::error::Result;
use crate::event::EventKind;
use crate::Store;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preference {
    pub winner: EntryId,
    pub loser: EntryId,
    /// Fixed at `1.0` for now -- INFRA.md's spec already names this
    /// ("corrections... weighted by confidence"), kept here so a future
    /// UI control (e.g. a confidence slider) doesn't need an event-shape
    /// migration, even though nothing sets it below `1.0` yet.
    pub confidence: f32,
}

impl Store {
    pub fn record_preference(
        &mut self,
        winner: &EntryId,
        loser: &EntryId,
        note: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let preference = Preference {
            winner: winner.clone(),
            loser: loser.clone(),
            confidence: 1.0,
        };
        let target = format!("{}:{}", winner.0, loser.0);
        append_event(
            &tx,
            EventKind::PreferenceRecord,
            Some(&target),
            None,
            to_json(Some(&preference))?.as_deref(),
            note,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every recorded preference, with entries merged away since read as
    /// the entries they were merged into.
    pub fn load_preferences(&self) -> Result<Vec<Preference>> {
        let merged = self.merged_into()?;
        let follow = |id: &mut EntryId| {
            if let Some(into) = merged.get(&id.0) {
                id.0 = into.clone();
            }
        };
        let mut preferences = Vec::new();
        for event in self.event_log()? {
            if event.kind != EventKind::PreferenceRecord.as_str() {
                continue;
            }
            let Some(after) = event.after else {
                continue;
            };
            let mut preference: Preference = serde_json::from_str(&after)?;
            follow(&mut preference.winner);
            follow(&mut preference.loser);
            preferences.push(preference);
        }
        Ok(preferences)
    }
}
