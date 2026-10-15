//! Per-entry exemptions from validation rules (the `waiver` table, V14):
//! the legacy `<validatorSuppress rules="...">`, and "not applicable"
//! as marked in the app. The rules themselves live with whoever checks
//! them; the store only remembers which entry opted out of which.

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use nrs_core::EntryId;

use crate::crud::append_event;
use crate::error::{Result, StoreError};
use crate::event::EventKind;
use crate::Store;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Waiver {
    pub entry: EntryId,
    pub rule: String,
    pub reason: Option<String>,
}

impl Store {
    /// Every waiver, by entry then rule.
    pub fn waivers(&self) -> Result<Vec<Waiver>> {
        let mut stmt = self
            .conn
            .prepare("SELECT entry, rule, reason FROM waiver ORDER BY entry, rule")?;
        let rows = stmt.query_map([], |row| {
            Ok(Waiver {
                entry: EntryId(row.get(0)?),
                rule: row.get(1)?,
                reason: row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }

    /// Exempts `entry` from `rule`, or updates the reason it is exempt.
    pub fn set_waiver(&mut self, entry: &EntryId, rule: &str, reason: Option<&str>) -> Result<()> {
        if rule.trim().is_empty() {
            return Err(StoreError::Invalid("a waiver needs a rule".into()));
        }
        let tx = self.conn.transaction()?;
        let before: Option<Option<String>> = tx
            .query_row(
                "SELECT reason FROM waiver WHERE entry = ?1 AND rule = ?2",
                params![entry.0, rule],
                |row| row.get(0),
            )
            .optional()?;
        if before.as_ref().map(Option::as_deref) == Some(reason) {
            return Ok(());
        }
        tx.execute(
            "INSERT INTO waiver (entry, rule, reason) VALUES (?1, ?2, ?3) \
             ON CONFLICT(entry, rule) DO UPDATE SET reason = excluded.reason",
            params![entry.0, rule, reason],
        )?;
        append_event(
            &tx,
            EventKind::WaiverSet,
            Some(&entry.0),
            before
                .map(|reason| serde_json::json!({ "rule": rule, "reason": reason }).to_string())
                .as_deref(),
            Some(&serde_json::json!({ "rule": rule, "reason": reason }).to_string()),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Holds `entry` to `rule` again. Not an error if it wasn't exempt.
    pub fn remove_waiver(&mut self, entry: &EntryId, rule: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before: Option<Option<String>> = tx
            .query_row(
                "SELECT reason FROM waiver WHERE entry = ?1 AND rule = ?2",
                params![entry.0, rule],
                |row| row.get(0),
            )
            .optional()?;
        let Some(reason) = before else {
            return Ok(());
        };
        tx.execute(
            "DELETE FROM waiver WHERE entry = ?1 AND rule = ?2",
            params![entry.0, rule],
        )?;
        append_event(
            &tx,
            EventKind::WaiverRemove,
            Some(&entry.0),
            Some(&serde_json::json!({ "rule": rule, "reason": reason }).to_string()),
            None,
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `(entry, rule)` pairs whose waiver was removed by hand, which a
    /// resync must not bring back.
    pub(crate) fn removed_waivers(&self) -> Result<std::collections::BTreeSet<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT target, json_extract(before, '$.rule') FROM event \
             WHERE kind = ?1 AND target IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![EventKind::WaiverRemove.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use nrs_core::EntryId;

    use crate::Store;

    fn store_with_entry() -> (Store, EntryId) {
        let mut store = Store::open_in_memory().unwrap();
        let id = store.add_entry("s", "1", Some("t")).unwrap();
        (store, id)
    }

    #[test]
    fn set_update_and_remove_round_trip() {
        let (mut store, id) = store_with_entry();
        store.set_waiver(&id, "dah-lone-pads", None).unwrap();
        store
            .set_waiver(&id, "dah-lone-pads", Some("paired elsewhere"))
            .unwrap();
        let waivers = store.waivers().unwrap();
        assert_eq!(waivers.len(), 1);
        assert_eq!(waivers[0].reason.as_deref(), Some("paired elsewhere"));

        store.remove_waiver(&id, "dah-lone-pads").unwrap();
        assert!(store.waivers().unwrap().is_empty());
        assert!(store
            .removed_waivers()
            .unwrap()
            .contains(&(id.0.clone(), "dah-lone-pads".to_string())));
        // Removing again is a no-op.
        store.remove_waiver(&id, "dah-lone-pads").unwrap();
    }

    #[test]
    fn waiver_needs_a_known_entry() {
        let mut store = Store::open_in_memory().unwrap();
        assert!(store
            .set_waiver(&EntryId("missing".into()), "dah-lone-pads", None)
            .is_err());
    }
}
