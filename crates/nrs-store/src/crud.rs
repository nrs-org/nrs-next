//! Mutations, each atomic with its `event` row append (INFRA.md §6): the
//! data write and the audit entry commit together or not at all.

use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use ulid::Ulid;

use nrs_core::{
    Anchor, AnchorId, Atom, Entry, EntryId, Impact, ImpactId, Isotonic, RelKind, Relation,
    TimeParams, Transform,
};

use crate::error::{Result, StoreError};
use crate::event::EventKind;
use crate::relkind::{rel_kind_from_str, rel_kind_to_str};
use crate::{Store, StoredParams, DEFAULT_MEDIUM};

fn now_ts() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn new_id() -> String {
    Ulid::new().to_string()
}

pub(crate) fn to_json<T: Serialize>(v: Option<&T>) -> Result<Option<String>> {
    v.map(serde_json::to_string).transpose().map_err(Into::into)
}

pub(crate) fn append_event(
    tx: &Transaction,
    kind: EventKind,
    target: Option<&str>,
    before_json: Option<&str>,
    after_json: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO event (ts, kind, target, before, after, note) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            now_ts(),
            kind.as_str(),
            target,
            before_json,
            after_json,
            note
        ],
    )?;
    Ok(())
}

fn insert_atom(tx: &Transaction, impact_id: &str, atom: &Atom) -> Result<()> {
    let blend = serde_json::to_string(&atom.blend)?;
    let time = serde_json::to_string(&atom.time)?;
    tx.execute(
        "INSERT INTO impact_atom (impact, source, blend, magnitude, time) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![impact_id, atom.source.0, blend, atom.magnitude, time],
    )?;
    Ok(())
}

fn read_relation(tx: &Transaction, id: &str) -> Result<Option<Relation>> {
    let row: Option<(String, String, String, String)> = tx
        .query_row(
            "SELECT kind, from_e, to_e, transform FROM relation WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((kind, from, to, transform)) = row else {
        return Ok(None);
    };
    Ok(Some(Relation {
        kind: rel_kind_from_str(&kind)?,
        from: EntryId(from),
        to: EntryId(to),
        transform: serde_json::from_str(&transform)?,
    }))
}

fn read_anchor(tx: &Transaction, id: &AnchorId) -> Result<Option<Anchor>> {
    tx.query_row(
        "SELECT label, family FROM anchor WHERE id = ?1",
        params![id.0],
        |row| {
            Ok(Anchor {
                id: id.clone(),
                label: row.get(0)?,
                family: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Inserts a minimal `anchor` row (label = the id itself) if one
/// doesn't already exist. See `set_kfeat_pair`'s doc comment.
fn ensure_anchor_exists(tx: &Transaction, id: &AnchorId) -> Result<()> {
    let exists: bool = tx
        .query_row("SELECT 1 FROM anchor WHERE id = ?1", params![id.0], |_| {
            Ok(())
        })
        .optional()?
        .is_some();
    if !exists {
        tx.execute(
            "INSERT INTO anchor (id, label, family) VALUES (?1, ?2, NULL)",
            params![id.0, id.0],
        )?;
    }
    Ok(())
}

/// A relation as the entry page needs to see it: the graded transform,
/// plus (when set) the DAH_standards params it was built from.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryRelation {
    pub id: String,
    pub kind: RelKind,
    pub from: EntryId,
    pub to: EntryId,
    pub transform: Transform,
    pub dah_params: Option<String>,
}

impl Store {
    /// Looks up the NRS entry that references one type-store entry.
    /// `store_id` is opaque: provider IDs and cross-provider identity
    /// remain private to the type store.
    pub fn find_entry(&self, store: &str, store_id: &str) -> Result<Option<EntryId>> {
        self.conn
            .query_row(
                "SELECT id FROM entry WHERE store = ?1 AND store_id = ?2",
                params![store, store_id],
                |row| row.get(0),
            )
            .optional()
            .map(|opt| opt.map(EntryId))
            .map_err(Into::into)
    }

    pub fn add_entry(
        &mut self,
        store: &str,
        store_id: &str,
        title_cache: Option<&str>,
    ) -> Result<EntryId> {
        let id = new_id();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO entry (id, store, store_id, title_cache) VALUES (?1, ?2, ?3, ?4)",
            params![id, store, store_id, title_cache],
        )?;
        let entry = Entry {
            id: EntryId(id.clone()),
            store: store.to_string(),
            store_id: store_id.to_string(),
            title_cache: title_cache.map(String::from),
        };
        append_event(
            &tx,
            EventKind::EntryAdd,
            Some(&id),
            None,
            to_json(Some(&entry))?.as_deref(),
            None,
        )?;
        tx.commit()?;
        Ok(EntryId(id))
    }

    /// Entries with a stored kind (see `set_entry_kind`), by entry id.
    pub fn entry_kinds(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind FROM entry WHERE kind IS NOT NULL")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }

    /// Stores what kind of work `id` is; `None` clears it back to
    /// inference.
    pub fn set_entry_kind(&mut self, id: &EntryId, kind: Option<&str>) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before: Option<Option<String>> = tx
            .query_row(
                "SELECT kind FROM entry WHERE id = ?1",
                params![id.0],
                |row| row.get(0),
            )
            .optional()?;
        let Some(before) = before else {
            return Err(StoreError::Invalid(format!("no entry {}", id.0)));
        };
        if before.as_deref() == kind {
            return Ok(());
        }
        tx.execute(
            "UPDATE entry SET kind = ?2 WHERE id = ?1",
            params![id.0, kind],
        )?;
        append_event(
            &tx,
            EventKind::EntryEdit,
            Some(&id.0),
            Some(&serde_json::json!({ "kind": before }).to_string()),
            Some(&serde_json::json!({ "kind": kind }).to_string()),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_impact(
        &mut self,
        target: &EntryId,
        atoms: &[Atom],
        note: Option<&str>,
    ) -> Result<ImpactId> {
        let id = new_id();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO impact (id, target, note, created) VALUES (?1, ?2, ?3, ?4)",
            params![id, target.0, note, now_ts()],
        )?;
        for atom in atoms {
            insert_atom(&tx, &id, atom)?;
        }
        let impact = Impact {
            id: ImpactId(id.clone()),
            target: target.clone(),
            atoms: atoms.to_vec(),
        };
        append_event(
            &tx,
            EventKind::ImpactAdd,
            Some(&id),
            None,
            to_json(Some(&impact))?.as_deref(),
            note,
        )?;
        tx.commit()?;
        Ok(ImpactId(id))
    }

    /// Records the DAH_standards kind + params an impact was built
    /// from (INFRA.md's "kind-based forms" UI), so the entry page can
    /// show that instead of a raw anchor blend. `factor` is the
    /// contributor share (1.0 for a plain manually-created impact) the
    /// kind editor multiplies the formula's output by before saving --
    /// see `dah_factor`'s schema comment. A separate call from
    /// `add_impact`/`edit_impact` rather than an extra parameter on
    /// them, since the atoms and the provenance are independently
    /// useful (tests, the raw chip editor) without one requiring the
    /// other. No event append -- this is presentation metadata, not a
    /// scoring-relevant change; the atom edit it accompanies already
    /// logs its own event.
    pub fn set_impact_dah(
        &mut self,
        id: &ImpactId,
        kind: &str,
        params_json: &str,
        factor: f32,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE impact SET dah_kind = ?2, dah_params = ?3, dah_factor = ?4 WHERE id = ?1",
            params![id.0, kind, params_json, factor],
        )?;
        Ok(())
    }

    pub fn edit_impact(&mut self, id: &ImpactId, atoms: &[Atom], note: Option<&str>) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before = crate::shared::read_record(&tx, id)?;
        tx.execute("DELETE FROM impact_atom WHERE impact = ?1", params![id.0])?;
        for atom in atoms {
            insert_atom(&tx, &id.0, atom)?;
        }
        tx.execute(
            "UPDATE impact SET note = ?2 WHERE id = ?1",
            params![id.0, note],
        )?;
        let after = crate::shared::read_record(&tx, id)?;
        append_event(
            &tx,
            EventKind::ImpactEdit,
            Some(&id.0),
            to_json(before.as_ref())?.as_deref(),
            to_json(after.as_ref())?.as_deref(),
            note,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Accepts the current review suggestion as final, for the `/review`
    /// UI's common "this looks right" path: if `impact.note` (the
    /// existing `SCHEMA_V1` column) is still unset, fills it in from the
    /// `ReviewLog`'s raw `source_comment` for this impact; clears the
    /// `needs_review` flag regardless of whether there was a comment to
    /// promote. Unlike `edit_impact`, this never touches the blend --
    /// confirming doesn't imply agreement with a specific anchor, just
    /// that nothing here needs a second look anymore.
    pub fn confirm_impact(&mut self, id: &ImpactId) -> Result<()> {
        let source_comment = self.review.get(&id.0).source_comment;

        let tx = self.conn.transaction()?;
        let before = crate::shared::read_record(&tx, id)?;
        let has_note: Option<String> = tx
            .query_row(
                "SELECT note FROM impact WHERE id = ?1",
                params![id.0],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if has_note.is_none() {
            if let Some(comment) = &source_comment {
                tx.execute(
                    "UPDATE impact SET note = ?2 WHERE id = ?1",
                    params![id.0, comment],
                )?;
            }
        }
        let after = crate::shared::read_record(&tx, id)?;
        append_event(
            &tx,
            EventKind::ImpactConfirm,
            Some(&id.0),
            to_json(before.as_ref())?.as_deref(),
            to_json(after.as_ref())?.as_deref(),
            None,
        )?;
        tx.commit()?;

        self.review.clear_needs_review(&id.0)?;
        Ok(())
    }

    pub fn delete_impact(&mut self, id: &ImpactId) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before = crate::shared::read_record(&tx, id)?;
        tx.execute("DELETE FROM impact_atom WHERE impact = ?1", params![id.0])?;
        tx.execute("DELETE FROM impact_member WHERE impact = ?1", params![id.0])?;
        tx.execute("DELETE FROM impact_split WHERE impact = ?1", params![id.0])?;
        tx.execute("DELETE FROM impact WHERE id = ?1", params![id.0])?;
        append_event(
            &tx,
            EventKind::ImpactDelete,
            Some(&id.0),
            to_json(before.as_ref())?.as_deref(),
            None,
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Returns the new relation's id. There's no `RelationId` in
    /// `nrs-core` (`Relation` carries no id — it's store-only addressing),
    /// so this hands back the raw ULID.
    pub fn add_relation(
        &mut self,
        kind: RelKind,
        from: &EntryId,
        to: &EntryId,
        transform: &Transform,
    ) -> Result<String> {
        let id = new_id();
        let tx = self.conn.transaction()?;
        let transform_json = serde_json::to_string(transform)?;
        tx.execute(
            "INSERT INTO relation (id, kind, from_e, to_e, transform) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, rel_kind_to_str(kind), from.0, to.0, transform_json],
        )?;
        let relation = Relation {
            kind,
            from: from.clone(),
            to: to.clone(),
            transform: transform.clone(),
        };
        append_event(
            &tx,
            EventKind::RelationAdd,
            Some(&id),
            None,
            to_json(Some(&relation))?.as_deref(),
            None,
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn edit_relation(
        &mut self,
        id: &str,
        kind: RelKind,
        from: &EntryId,
        to: &EntryId,
        transform: &Transform,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before = read_relation(&tx, id)?;
        let transform_json = serde_json::to_string(transform)?;
        tx.execute(
            "UPDATE relation SET kind = ?2, from_e = ?3, to_e = ?4, transform = ?5 \
             WHERE id = ?1",
            params![id, rel_kind_to_str(kind), from.0, to.0, transform_json],
        )?;
        let after = Relation {
            kind,
            from: from.clone(),
            to: to.clone(),
            transform: transform.clone(),
        };
        append_event(
            &tx,
            EventKind::RelationEdit,
            Some(id),
            to_json(before.as_ref())?.as_deref(),
            to_json(Some(&after))?.as_deref(),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// See `set_impact_dah` -- same idea, but `relation.kind` already
    /// doubles as the DAH kind for the four relation kinds
    /// DAH_standards covers, so only the params need a dedicated column.
    pub fn set_relation_dah(&mut self, id: &str, params_json: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE relation SET dah_params = ?2 WHERE id = ?1",
            params![id, params_json],
        )?;
        Ok(())
    }

    /// Every relation touching `entry` on either side, in insertion
    /// order -- the entry page's unified impact/relation view needs
    /// both directions (an entry can be a `Contains` parent or child,
    /// a `killed_by` source or target, ...).
    pub fn relations_for_entry(&self, entry: &EntryId) -> Result<Vec<EntryRelation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, from_e, to_e, transform, dah_params FROM relation \
             WHERE from_e = ?1 OR to_e = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![entry.0], |row| {
            let id: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let from: String = row.get(2)?;
            let to: String = row.get(3)?;
            let transform: String = row.get(4)?;
            let dah_params: Option<String> = row.get(5)?;
            Ok((id, kind, from, to, transform, dah_params))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, from, to, transform, dah_params) = row?;
            out.push(EntryRelation {
                id,
                kind: rel_kind_from_str(&kind)?,
                from: EntryId(from),
                to: EntryId(to),
                transform: serde_json::from_str(&transform)?,
                dah_params,
            });
        }
        Ok(out)
    }

    pub fn delete_relation(&mut self, id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before = read_relation(&tx, id)?;
        tx.execute("DELETE FROM relation WHERE id = ?1", params![id])?;
        append_event(
            &tx,
            EventKind::RelationDelete,
            Some(id),
            to_json(before.as_ref())?.as_deref(),
            None,
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_anchor(
        &mut self,
        id: &AnchorId,
        label: &str,
        family: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before = read_anchor(&tx, id)?;
        tx.execute(
            "INSERT INTO anchor (id, label, family) VALUES (?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET label = excluded.label, family = excluded.family",
            params![id.0, label, family],
        )?;
        let after = Anchor {
            id: id.clone(),
            label: label.to_string(),
            family: family.map(String::from),
        };
        append_event(
            &tx,
            EventKind::AnchorUpsert,
            Some(&id.0),
            to_json(before.as_ref())?.as_deref(),
            to_json(Some(&after))?.as_deref(),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Sets one entry of the pooling-exponent matrix `K`
    /// (`nrs_core::KFeatMatrix`): `a == b` sets that anchor's own
    /// exponent, otherwise the exponent the pair pools at. Must lie in
    /// `(0, 1]`; 1 is the default (additive).
    pub fn set_kfeat_pair(&mut self, a: &AnchorId, b: &AnchorId, k: f32) -> Result<()> {
        if !(k > 0.0 && k <= 1.0) {
            return Err(StoreError::InvalidData(format!(
                "kfeat exponent {k} for {}:{} is outside (0, 1]",
                a.0, b.0
            )));
        }
        let tx = self.conn.transaction()?;
        // `kfeat.a`/`kfeat.b` are FK-constrained to `anchor(id)`, but
        // `nrs-import` doesn't populate `Palette.anchors` yet -- so an
        // anchor id that only ever appeared in an atom's `blend` (the
        // common case for imported data) wouldn't exist as a row here.
        // Auto-register it minimally rather than making every caller of
        // this method know about that gap; this is plumbing to satisfy
        // the constraint, not a user action, so it doesn't get its own
        // event (the `kfeat.set` event below is the meaningful one).
        ensure_anchor_exists(&tx, a)?;
        ensure_anchor_exists(&tx, b)?;
        let before: Option<f32> = tx
            .query_row(
                "SELECT k FROM kfeat WHERE a = ?1 AND b = ?2",
                params![a.0, b.0],
                |row| row.get(0),
            )
            .optional()?;
        tx.execute(
            "INSERT INTO kfeat (a, b, k) VALUES (?1, ?2, ?3) \
             ON CONFLICT(a, b) DO UPDATE SET k = excluded.k",
            params![a.0, b.0, k],
        )?;
        let target = format!("{}:{}", a.0, b.0);
        append_event(
            &tx,
            EventKind::KfeatSet,
            Some(&target),
            to_json(before.as_ref())?.as_deref(),
            to_json(Some(&k))?.as_deref(),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Sets how dated mass decorrelates over time (`nrs_core::TimeParams`).
    pub fn set_time_params(&mut self, time: &TimeParams) -> Result<()> {
        if !((0.0..=1.0).contains(&time.shared) && time.length_days > 0.0) {
            return Err(StoreError::InvalidData(format!(
                "time params need shared in [0, 1] and length_days > 0, got {time:?}"
            )));
        }
        let tx = self.conn.transaction()?;
        let before: Option<String> = tx
            .query_row("SELECT json FROM params WHERE id = 1", [], |row| row.get(0))
            .optional()?;
        let mut stored: StoredParams = match &before {
            Some(json) => serde_json::from_str(json)?,
            None => StoredParams::default(),
        };
        stored.time = *time;
        let after = serde_json::to_string(&stored)?;
        tx.execute(
            "INSERT INTO params (id, json) VALUES (1, ?1) \
             ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![after],
        )?;
        append_event(
            &tx,
            EventKind::ParamSet,
            None,
            before.as_deref(),
            Some(&after),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_calibration(&mut self, isotonic: &Isotonic) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before: Option<String> = tx
            .query_row(
                "SELECT isotonic FROM calibration WHERE medium = ?1",
                [DEFAULT_MEDIUM],
                |row| row.get(0),
            )
            .optional()?;
        let after = serde_json::to_string(isotonic)?;
        tx.execute(
            "INSERT INTO calibration (medium, isotonic) VALUES (?1, ?2) \
             ON CONFLICT(medium) DO UPDATE SET isotonic = excluded.isotonic",
            params![DEFAULT_MEDIUM, after],
        )?;
        append_event(
            &tx,
            EventKind::CalibrationSet,
            Some(DEFAULT_MEDIUM),
            before.as_deref(),
            Some(&after),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use nrs_core::{AnchorId, Atom, Entry, Palette, Snapshot, SourceId, TimeSupport};

    use super::*;
    use crate::ImportExtras;

    fn sample_atom(anchor: &str, source: &str, magnitude: f32) -> Atom {
        Atom {
            source: SourceId(source.into()),
            blend: vec![(AnchorId(anchor.into()), 1.0)],
            magnitude,
            time: TimeSupport::Moment,
        }
    }

    #[test]
    fn set_entry_kind_stores_logs_and_clears() {
        let mut store = Store::open_in_memory().unwrap();
        let id = store.add_entry("url", "https://youtu.be/x", None).unwrap();
        assert!(store.entry_kinds().unwrap().is_empty());

        store.set_entry_kind(&id, Some("track")).unwrap();
        assert_eq!(store.entry_kinds().unwrap()[&id.0], "track");
        let (before, after): (String, String) = store
            .conn
            .query_row(
                "SELECT before, after FROM event WHERE kind = 'entry.edit' AND target = ?1",
                params![id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (before.as_str(), after.as_str()),
            (r#"{"kind":null}"#, r#"{"kind":"track"}"#)
        );

        store.set_entry_kind(&id, None).unwrap();
        assert!(store.entry_kinds().unwrap().is_empty());
        assert!(store
            .set_entry_kind(&EntryId("missing".into()), Some("track"))
            .is_err());
    }

    fn read_note(store: &Store, id: &ImpactId) -> Option<String> {
        store
            .conn
            .query_row(
                "SELECT note FROM impact WHERE id = ?1",
                params![id.0],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Seeds a store the way `nrs-import`'s bulk path does (fixed ids,
    /// staging state in `ReviewLog`), since that's the only route by
    /// which a `source_comment` gets attached today -- `add_impact`
    /// writes straight to the confirmed `note` column instead.
    fn imported_store(comment: Option<&str>, needs_review: bool) -> (Store, ImpactId) {
        let mut store = Store::open_in_memory().unwrap();
        let entry = EntryId("e1".into());
        let impact_id = ImpactId("i1".into());
        let snap = Snapshot {
            entries: vec![Entry {
                id: entry.clone(),
                store: "music".into(),
                store_id: "m1".into(),
                title_cache: None,
            }],
            impacts: vec![Impact {
                id: impact_id.clone(),
                target: entry,
                atoms: vec![sample_atom("gut-punch", "src-a", 1.0)],
            }],
            relations: vec![],
            palette: Palette {
                anchors: vec![],
                k_feat: Default::default(),
            },
        };
        let mut comments = BTreeMap::new();
        if let Some(comment) = comment {
            comments.insert(impact_id.clone(), comment.to_string());
        }
        let mut needs_review_set = BTreeSet::new();
        if needs_review {
            needs_review_set.insert(impact_id.clone());
        }
        store
            .import_snapshot(
                &snap,
                &ImportExtras {
                    comments,
                    needs_review: needs_review_set,
                    ..Default::default()
                },
            )
            .unwrap();
        (store, impact_id)
    }

    #[test]
    fn confirm_impact_promotes_unconfirmed_comment_to_note_and_clears_flag() {
        let (mut store, impact_id) = imported_store(Some("the hype is unreal"), true);

        store.confirm_impact(&impact_id).unwrap();

        assert_eq!(
            read_note(&store, &impact_id).as_deref(),
            Some("the hype is unreal")
        );
        assert!(!store.review.get(&impact_id.0).needs_review);
    }

    #[test]
    fn confirm_impact_does_not_overwrite_an_already_confirmed_note() {
        let (mut store, impact_id) = imported_store(Some("the hype is unreal"), true);
        store
            .edit_impact(
                &impact_id,
                &[sample_atom("gut-punch", "src-a", 1.0)],
                Some("actually just comedic timing"),
            )
            .unwrap();

        store.confirm_impact(&impact_id).unwrap();

        assert_eq!(
            read_note(&store, &impact_id).as_deref(),
            Some("actually just comedic timing")
        );
    }

    #[test]
    fn confirm_impact_with_no_comment_just_clears_the_flag() {
        let (mut store, impact_id) = imported_store(None, true);

        store.confirm_impact(&impact_id).unwrap();

        assert_eq!(read_note(&store, &impact_id), None);
        assert!(!store.review.get(&impact_id.0).needs_review);
    }

    #[test]
    fn edit_impact_writes_the_note_column() {
        let mut store = Store::open_in_memory().unwrap();
        let entry = store.add_entry("music", "m1", None).unwrap();
        let impact_id = store
            .add_impact(&entry, &[sample_atom("gut-punch", "src-a", 5.0)], None)
            .unwrap();

        store
            .edit_impact(
                &impact_id,
                &[sample_atom("gut-punch", "src-a", 5.0)],
                Some("confirmed: genuine despair"),
            )
            .unwrap();

        assert_eq!(
            read_note(&store, &impact_id).as_deref(),
            Some("confirmed: genuine despair")
        );
    }
}
