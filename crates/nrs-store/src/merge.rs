//! Merging one entry into another: everything that names `from` is made
//! to name `into`, and `from` goes away.
//!
//! A merge is a plain union. NRS puts no limit on how many impacts of a
//! kind an entry carries, so nothing is summed, dropped or picked by
//! impact kind here; deciding that two of the merged entry's impacts say
//! the same thing is left to whoever knows that (a type store's own
//! conventions, or a person). The store only resolves what its schema
//! forces:
//!
//! - A relation between the two entries would relate the merged entry to
//!   itself, so it is dropped.
//! - A shared impact both entries contribute to keeps one member row for
//!   `into`, holding the two shares added together. A stored split tree
//!   naming both is dropped (it would name one member twice); the app
//!   infers a fresh one from the flat shares.
//! - A waiver both entries hold keeps `into`'s reason.
//! - Progress is one row per entry: `from`'s moves over when `into` has
//!   none, and two rows are a conflict the merge refuses.
//! - `into` keeps its binding and title, and takes `from`'s kind only
//!   when it has none.
//! - The form params kept beside impacts and relations (`dah_params`)
//!   name entries too (an impact's `source`, a relation's `from_entry`),
//!   and are rewritten like the columns, so editing a moved impact in its
//!   form doesn't point it back at `from`.
//!
//! The merge is one `entry.merge` event naming `from`, with the plan as
//! its `before` and `{"into": ...}` as its `after`. Those events are also
//! the redirect table (`merged_into`): preferences recorded against
//! `from` follow it, and a resync maps a re-imported `from` onto `into`
//! instead of adding it back (`follow_merges`).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;

use nrs_core::{Entry, EntryId, Snapshot};

use crate::crud::append_event;
use crate::error::{Result, StoreError};
use crate::event::EventKind;
use crate::import::ImportExtras;
use crate::split::SplitNode;
use crate::Store;

/// What `merge_entries` does, as `plan_merge` reports it beforehand.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergePlan {
    /// The entry that goes away.
    pub from: Entry,
    pub into: Entry,
    /// Impacts targeting `from`, retargeted to `into`.
    pub impacts: Vec<String>,
    /// Atoms whose source is `from`, on any impact.
    pub atoms: usize,
    /// Impacts and relations whose form params name `from`.
    pub impact_params: Vec<String>,
    pub relation_params: Vec<String>,
    /// Shared impacts `from` contributes to.
    pub members: Vec<String>,
    /// Of those, the ones `into` contributes to as well: its share
    /// becomes the sum of both.
    pub combined_members: Vec<String>,
    /// Stored split trees naming `from` that are rewritten to `into`.
    pub splits: Vec<String>,
    /// Stored split trees naming both, dropped for an inferred one.
    pub dropped_splits: Vec<String>,
    /// Relations repointed from `from` to `into`.
    pub relations: Vec<String>,
    /// Relations between the two entries, dropped.
    pub dropped_relations: Vec<String>,
    /// Rules `from` is waived from that move to `into`.
    pub waivers: Vec<String>,
    /// Rules both are waived from; `into`'s waiver is kept.
    pub duplicate_waivers: Vec<String>,
    /// Whether `from`'s progress row moves to `into`.
    pub progress_moved: bool,
    /// `from`'s kind, when `into` has none and takes it.
    pub kind: Option<String>,
    /// Why the merge cannot run; `merge_entries` refuses a plan with any.
    pub conflicts: Vec<String>,
}

fn read_entry(tx: &Transaction, id: &EntryId) -> Result<Option<(Entry, Option<String>)>> {
    tx.query_row(
        "SELECT id, store, store_id, title_cache, kind FROM entry WHERE id = ?1",
        params![id.0],
        |row| {
            Ok((
                Entry {
                    id: EntryId(row.get(0)?),
                    store: row.get(1)?,
                    store_id: row.get(2)?,
                    title_cache: row.get(3)?,
                },
                row.get(4)?,
            ))
        },
    )
    .optional()
    .map_err(Into::into)
}

fn strings(tx: &Transaction, sql: &str, params: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(sql)?;
    let rows = stmt.query_map(params, |row| row.get(0))?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

/// Replaces member `from` with `into` throughout a split tree.
fn rename_member(node: &mut SplitNode, from: &EntryId, into: &EntryId) {
    match node {
        SplitNode::Member { entry, .. } => {
            if entry == from {
                *entry = into.clone();
            }
        }
        SplitNode::Group { children, .. } => {
            for child in children {
                rename_member(child, from, into);
            }
        }
    }
}

/// `(id, params)` of every `table` row whose `dah_params` object has a
/// top-level value naming `entry`, with those values renamed to `into`.
fn renamed_params(
    tx: &Transaction,
    table: &str,
    entry: &EntryId,
    into: &EntryId,
) -> Result<Vec<(String, String)>> {
    let mut stmt = tx.prepare(&format!(
        "SELECT id, dah_params FROM {table} WHERE instr(dah_params, ?1) > 0 ORDER BY rowid"
    ))?;
    let rows = stmt.query_map(params![format!("\"{}\"", entry.0)], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, json) = row?;
        let Ok(serde_json::Value::Object(mut fields)) = serde_json::from_str(&json) else {
            continue;
        };
        let mut renamed = false;
        for value in fields.values_mut() {
            if value.as_str() == Some(entry.0.as_str()) {
                *value = into.0.clone().into();
                renamed = true;
            }
        }
        if renamed {
            out.push((id, serde_json::Value::Object(fields).to_string()));
        }
    }
    Ok(out)
}

fn names_member(node: &SplitNode, id: &EntryId) -> bool {
    match node {
        SplitNode::Member { entry, .. } => entry == id,
        SplitNode::Group { children, .. } => children.iter().any(|c| names_member(c, id)),
    }
}

fn plan(tx: &Transaction, from: &EntryId, into: &EntryId) -> Result<MergePlan> {
    if from == into {
        return Err(StoreError::Invalid(format!(
            "cannot merge entry {} into itself",
            from.0
        )));
    }
    let (from_entry, from_kind) =
        read_entry(tx, from)?.ok_or_else(|| StoreError::Invalid(format!("no entry {}", from.0)))?;
    let (into_entry, into_kind) =
        read_entry(tx, into)?.ok_or_else(|| StoreError::Invalid(format!("no entry {}", into.0)))?;

    let impacts = strings(
        tx,
        "SELECT id FROM impact WHERE target = ?1 ORDER BY rowid",
        params![from.0],
    )?;
    let atoms: usize = tx.query_row(
        "SELECT COUNT(*) FROM impact_atom WHERE source = ?1",
        params![from.0],
        |row| row.get(0),
    )?;
    let ids = |rows: Vec<(String, String)>| rows.into_iter().map(|(id, _)| id).collect();
    let impact_params = ids(renamed_params(tx, "impact", from, into)?);
    let relation_params = ids(renamed_params(tx, "relation", from, into)?);
    let members = strings(
        tx,
        "SELECT impact FROM impact_member WHERE contributor = ?1 ORDER BY impact",
        params![from.0],
    )?;
    let combined_members = strings(
        tx,
        "SELECT a.impact FROM impact_member a JOIN impact_member b ON b.impact = a.impact \
         WHERE a.contributor = ?1 AND b.contributor = ?2 ORDER BY a.impact",
        params![from.0, into.0],
    )?;

    let mut splits = Vec::new();
    let mut dropped_splits = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT impact, tree FROM impact_split ORDER BY impact")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (impact, tree) = row?;
            let tree: SplitNode = serde_json::from_str(&tree)?;
            if !names_member(&tree, from) {
                continue;
            }
            if names_member(&tree, into) {
                dropped_splits.push(impact);
            } else {
                splits.push(impact);
            }
        }
    }

    let between = "(from_e = ?1 AND to_e = ?2) OR (from_e = ?2 AND to_e = ?1)";
    let dropped_relations = strings(
        tx,
        &format!("SELECT id FROM relation WHERE {between} ORDER BY rowid"),
        params![from.0, into.0],
    )?;
    let relations = strings(
        tx,
        &format!(
            "SELECT id FROM relation WHERE (from_e = ?1 OR to_e = ?1) AND NOT ({between}) \
             ORDER BY rowid"
        ),
        params![from.0, into.0],
    )?;

    let into_rules: BTreeSet<String> = strings(
        tx,
        "SELECT rule FROM waiver WHERE entry = ?1",
        params![into.0],
    )?
    .into_iter()
    .collect();
    let (duplicate_waivers, waivers): (Vec<String>, Vec<String>) = strings(
        tx,
        "SELECT rule FROM waiver WHERE entry = ?1 ORDER BY rule",
        params![from.0],
    )?
    .into_iter()
    .partition(|rule| into_rules.contains(rule));

    let has_progress = |id: &EntryId| -> Result<bool> {
        Ok(tx
            .query_row(
                "SELECT 1 FROM progress WHERE entry = ?1",
                params![id.0],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    let mut conflicts = Vec::new();
    let progress_moved = match (has_progress(from)?, has_progress(into)?) {
        (true, true) => {
            conflicts.push(format!(
                "both entries have progress; clear one of them before merging"
            ));
            false
        }
        (from_has, _) => from_has,
    };

    Ok(MergePlan {
        from: from_entry,
        into: into_entry,
        impacts,
        atoms,
        impact_params,
        relation_params,
        members,
        combined_members,
        splits,
        dropped_splits,
        relations,
        dropped_relations,
        waivers,
        duplicate_waivers,
        progress_moved,
        kind: if into_kind.is_none() { from_kind } else { None },
        conflicts,
    })
}

impl Store {
    /// What merging `from` into `into` would do, without writing.
    pub fn plan_merge(&self, from: &EntryId, into: &EntryId) -> Result<MergePlan> {
        // One consistent read; dropped without commit.
        let tx = self.conn.unchecked_transaction()?;
        plan(&tx, from, into)
    }

    /// Merges `from` into `into` (see the module docs) as one
    /// `entry.merge` event, and returns what it did.
    pub fn merge_entries(
        &mut self,
        from: &EntryId,
        into: &EntryId,
        note: Option<&str>,
    ) -> Result<MergePlan> {
        let tx = self.conn.transaction()?;
        let plan = plan(&tx, from, into)?;
        if !plan.conflicts.is_empty() {
            return Err(StoreError::Invalid(plan.conflicts.join("; ")));
        }
        let (f, i) = (from.0.as_str(), into.0.as_str());

        tx.execute(
            "UPDATE impact SET target = ?2 WHERE target = ?1",
            params![f, i],
        )?;
        tx.execute(
            "UPDATE impact_atom SET source = ?2 WHERE source = ?1",
            params![f, i],
        )?;
        for impact in &plan.combined_members {
            tx.execute(
                "UPDATE impact_member SET factor = factor + \
                 (SELECT factor FROM impact_member WHERE impact = ?1 AND contributor = ?2) \
                 WHERE impact = ?1 AND contributor = ?3",
                params![impact, f, i],
            )?;
            tx.execute(
                "DELETE FROM impact_member WHERE impact = ?1 AND contributor = ?2",
                params![impact, f],
            )?;
        }
        tx.execute(
            "UPDATE impact_member SET contributor = ?2 WHERE contributor = ?1",
            params![f, i],
        )?;
        for impact in &plan.splits {
            let tree: String = tx.query_row(
                "SELECT tree FROM impact_split WHERE impact = ?1",
                params![impact],
                |row| row.get(0),
            )?;
            let mut tree: SplitNode = serde_json::from_str(&tree)?;
            rename_member(&mut tree, from, into);
            tx.execute(
                "UPDATE impact_split SET tree = ?2 WHERE impact = ?1",
                params![impact, serde_json::to_string(&tree)?],
            )?;
        }
        for impact in &plan.dropped_splits {
            tx.execute(
                "DELETE FROM impact_split WHERE impact = ?1",
                params![impact],
            )?;
        }

        for relation in &plan.dropped_relations {
            tx.execute("DELETE FROM relation WHERE id = ?1", params![relation])?;
        }
        tx.execute(
            "UPDATE relation SET from_e = ?2 WHERE from_e = ?1",
            params![f, i],
        )?;
        tx.execute(
            "UPDATE relation SET to_e = ?2 WHERE to_e = ?1",
            params![f, i],
        )?;
        for table in ["impact", "relation"] {
            for (id, params_json) in renamed_params(&tx, table, from, into)? {
                tx.execute(
                    &format!("UPDATE {table} SET dah_params = ?2 WHERE id = ?1"),
                    params![id, params_json],
                )?;
            }
        }

        for rule in &plan.duplicate_waivers {
            tx.execute(
                "DELETE FROM waiver WHERE entry = ?1 AND rule = ?2",
                params![f, rule],
            )?;
        }
        tx.execute(
            "UPDATE waiver SET entry = ?2 WHERE entry = ?1",
            params![f, i],
        )?;
        tx.execute(
            "UPDATE progress SET entry = ?2 WHERE entry = ?1",
            params![f, i],
        )?;
        if let Some(kind) = &plan.kind {
            tx.execute("UPDATE entry SET kind = ?2 WHERE id = ?1", params![i, kind])?;
        }
        tx.execute("DELETE FROM entry WHERE id = ?1", params![f])?;

        append_event(
            &tx,
            EventKind::EntryMerge,
            Some(f),
            Some(&serde_json::to_string(&plan)?),
            Some(&serde_json::json!({ "into": i }).to_string()),
            note,
        )?;
        tx.commit()?;
        Ok(plan)
    }

    /// Merged-away entry id -> the entry it now lives on, following
    /// merges of merges to the end.
    pub fn merged_into(&self) -> Result<BTreeMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT target, json_extract(after, '$.into') FROM event \
             WHERE kind = ?1 AND target IS NOT NULL ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![EventKind::EntryMerge.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let direct: BTreeMap<String, String> = rows.collect::<std::result::Result<_, _>>()?;
        // Merging into a merged-away entry is refused, so chains end.
        Ok(direct
            .keys()
            .map(|from| {
                let mut to = &direct[from];
                while let Some(next) = direct.get(to) {
                    to = next;
                }
                (from.clone(), to.clone())
            })
            .collect())
    }

    /// Rewrites a fresh import so entries merged away since the last one
    /// are read as the entries they were merged into, before
    /// `plan_resync` matches it against the store: their entries are
    /// left out, and everything naming them names the survivor. Without
    /// this a resync would add a merged-away entry back, with copies of
    /// the impacts and relations that now live on the survivor.
    pub fn follow_merges(&self, snap: &mut Snapshot, extras: &mut ImportExtras) -> Result<()> {
        let merged = self.merged_into()?;
        if merged.is_empty() {
            return Ok(());
        }
        let map = |id: &mut String| {
            if let Some(into) = merged.get(id.as_str()) {
                *id = into.clone();
            }
        };
        snap.entries
            .retain(|entry| !merged.contains_key(&entry.id.0));
        for impact in &mut snap.impacts {
            map(&mut impact.target.0);
            for atom in &mut impact.atoms {
                map(&mut atom.source.0);
            }
        }
        for relation in &mut snap.relations {
            map(&mut relation.from.0);
            map(&mut relation.to.0);
        }
        snap.relations
            .retain(|relation| relation.from != relation.to);
        extras.credit_roles = std::mem::take(&mut extras.credit_roles)
            .into_iter()
            .map(|((mut a, mut b), roles)| {
                map(&mut a.0);
                map(&mut b.0);
                ((a, b), roles)
            })
            .collect();
        let mut waivers: BTreeMap<EntryId, BTreeSet<String>> = BTreeMap::new();
        for (mut entry, rules) in std::mem::take(&mut extras.waivers) {
            map(&mut entry.0);
            waivers.entry(entry).or_default().extend(rules);
        }
        extras.waivers = waivers;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use nrs_core::{AnchorId, Atom, ImpactId, RelKind, SourceId, TimeSupport, Transform};

    use crate::shared::ImpactMember;
    use crate::{Progress, ProgressStatus, ProgressUnit};

    use super::*;

    fn atom(source: &EntryId, anchor: &str, magnitude: f32) -> Atom {
        Atom {
            source: SourceId(source.0.clone()),
            blend: vec![(AnchorId(anchor.into()), 1.0)],
            magnitude,
            time: TimeSupport::Moment,
        }
    }

    fn music(store: &mut Store, entry: &EntryId, magnitude: f32) -> ImpactId {
        let id = store
            .add_impact(entry, &[atom(entry, "music", magnitude)], None)
            .unwrap();
        let params = serde_json::json!({ "source": entry.0, "music_base": "0.7" });
        store
            .set_impact_dah(&id, "music", &params.to_string(), 1.0)
            .unwrap();
        id
    }

    fn entry_ids(store: &Store) -> Vec<String> {
        store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|e| e.id.0)
            .collect()
    }

    #[test]
    fn merge_keeps_every_impact_and_drops_only_self_relations() {
        let mut store = Store::open_in_memory().unwrap();
        let old = store
            .add_entry("url", "https://mb/old", Some("Kitto"))
            .unwrap();
        let new = store
            .add_entry("url", "https://yt/new", Some("Kitto (MV)"))
            .unwrap();
        store.set_entry_kind(&new, Some("track")).unwrap();
        let album = store
            .add_entry("url", "https://mb/album", Some("Album"))
            .unwrap();
        let remix = store
            .add_entry("url", "https://mb/remix", Some("Remix"))
            .unwrap();

        let old_music = music(&mut store, &old, 2.1);
        let new_music = music(&mut store, &new, 1.95);
        let identity = Transform::Scalar(1.0);
        store
            .add_relation(RelKind::Contains, &album, &old, &identity)
            .unwrap();
        let remix_old = store
            .add_relation(RelKind::Remix, &old, &remix, &Transform::Scalar(0.2))
            .unwrap();
        let remix_new = store
            .add_relation(RelKind::Remix, &new, &remix, &Transform::Scalar(0.2))
            .unwrap();
        let ends = serde_json::json!({ "from_entry": new.0, "to_entry": remix.0 });
        store
            .set_relation_dah(&remix_new, &ends.to_string())
            .unwrap();
        let between = store
            .add_relation(RelKind::Remix, &new, &old, &identity)
            .unwrap();
        store
            .set_waiver(&new, "dah-no-music-impact", Some("new"))
            .unwrap();
        store
            .set_waiver(&old, "dah-no-music-impact", Some("old"))
            .unwrap();
        store.set_waiver(&new, "dah-lone-pads", None).unwrap();

        let preview = store.plan_merge(&new, &old).unwrap();
        assert_eq!(entry_ids(&store).len(), 4, "a plan writes nothing");
        let plan = store.merge_entries(&new, &old, Some("same song")).unwrap();
        assert_eq!(plan, preview);
        assert_eq!(plan.impacts, vec![new_music.0.clone()]);
        assert_eq!(plan.atoms, 1);
        assert_eq!(plan.impact_params, vec![new_music.0.clone()]);
        assert_eq!(plan.relation_params, vec![remix_new.clone()]);
        assert_eq!(plan.relations, vec![remix_new.clone()]);
        assert_eq!(plan.dropped_relations, vec![between]);
        assert_eq!(plan.waivers, vec!["dah-lone-pads".to_string()]);
        assert_eq!(
            plan.duplicate_waivers,
            vec!["dah-no-music-impact".to_string()]
        );
        assert_eq!(plan.kind.as_deref(), Some("track"));

        assert!(!entry_ids(&store).contains(&new.0));
        // Both music impacts survive: picking one is not the store's call.
        let (snapshot, _) = store.load_snapshot().unwrap();
        let on_old: BTreeSet<&str> = snapshot
            .impacts
            .iter()
            .filter(|i| i.target == old)
            .map(|i| i.id.0.as_str())
            .collect();
        assert_eq!(
            on_old,
            BTreeSet::from([old_music.0.as_str(), new_music.0.as_str()])
        );
        assert!(snapshot
            .impacts
            .iter()
            .flat_map(|i| &i.atoms)
            .all(|a| a.source.0 != new.0));
        // Form params follow too, so re-editing doesn't point back at `new`.
        for (entry, _, params) in store.impact_kinds_by_entry().unwrap() {
            let params: serde_json::Value = serde_json::from_str(&params.unwrap()).unwrap();
            assert_eq!(params["source"], serde_json::json!(entry));
        }
        let moved = store
            .relations_for_entry(&old)
            .unwrap()
            .into_iter()
            .find(|r| r.id == remix_new)
            .unwrap();
        let ends: serde_json::Value = serde_json::from_str(&moved.dah_params.unwrap()).unwrap();
        assert_eq!(ends["from_entry"], serde_json::json!(old.0));
        // The two remix relations are now identical, and both kept.
        let remixes: Vec<String> = store
            .relations_for_entry(&old)
            .unwrap()
            .into_iter()
            .filter(|r| r.kind == RelKind::Remix)
            .map(|r| r.id)
            .collect();
        assert_eq!(remixes, vec![remix_old, remix_new]);
        let waivers = store.waivers().unwrap();
        assert_eq!(waivers.len(), 2);
        assert!(waivers.iter().all(|w| w.entry == old));
        assert_eq!(
            waivers
                .iter()
                .find(|w| w.rule == "dah-no-music-impact")
                .and_then(|w| w.reason.as_deref()),
            Some("old")
        );
        assert_eq!(
            store.entry_kinds().unwrap().get(&old.0).map(String::as_str),
            Some("track")
        );

        let merges = store.merged_into().unwrap();
        assert_eq!(merges.get(&new.0), Some(&old.0));
        assert!(
            store.merge_entries(&new, &old, None).is_err(),
            "already gone"
        );
    }

    #[test]
    fn shared_members_add_up_and_progress_moves_or_conflicts() {
        let mut store = Store::open_in_memory().unwrap();
        let a = store.add_entry("s", "a", None).unwrap();
        let b = store.add_entry("s", "b", None).unwrap();
        let c = store.add_entry("s", "c", None).unwrap();
        let shared = store
            .add_impact(&c, &[atom(&c, "cozy", 1.0)], None)
            .unwrap();
        store.make_impact_shared(&shared).unwrap();
        let members = |pairs: &[(&EntryId, f32)]| -> Vec<ImpactMember> {
            pairs
                .iter()
                .map(|(e, f)| ImpactMember {
                    contributor: (*e).clone(),
                    factor: *f,
                })
                .collect()
        };
        store
            .edit_shared_impact(
                &shared,
                &[atom(&c, "cozy", 1.0)],
                &members(&[(&a, 0.25), (&b, 0.5), (&c, 0.25)]),
                None,
            )
            .unwrap();

        store
            .set_progress(
                &Progress::new(a.clone(), ProgressStatus::Active, ProgressUnit::Minute),
                None,
            )
            .unwrap();
        store
            .set_progress(
                &Progress::new(b.clone(), ProgressStatus::Active, ProgressUnit::Minute),
                None,
            )
            .unwrap();
        let blocked = store.plan_merge(&a, &b).unwrap();
        assert_eq!(blocked.conflicts.len(), 1);
        assert!(store.merge_entries(&a, &b, None).is_err());
        assert!(
            store.progress(&a).unwrap().is_some(),
            "a refused merge writes nothing"
        );

        store.clear_progress(&b).unwrap();
        let plan = store.merge_entries(&a, &b, None).unwrap();
        assert!(plan.progress_moved);
        assert_eq!(plan.combined_members, vec![shared.0.clone()]);
        assert!(store.progress(&b).unwrap().is_some());
        let members: BTreeMap<String, f32> = store
            .impact_members(&shared)
            .unwrap()
            .into_iter()
            .map(|m| (m.contributor.0, m.factor))
            .collect();
        assert_eq!(
            members,
            BTreeMap::from([(b.0.clone(), 0.75), (c.0.clone(), 0.25)])
        );
    }

    #[test]
    fn merges_chain_and_resync_input_follows_them() {
        let mut store = Store::open_in_memory().unwrap();
        let a = store.add_entry("s", "a", None).unwrap();
        let b = store.add_entry("s", "b", None).unwrap();
        let c = store.add_entry("s", "c", None).unwrap();
        store.merge_entries(&a, &b, None).unwrap();
        store.merge_entries(&b, &c, None).unwrap();
        let merged = store.merged_into().unwrap();
        assert_eq!(merged.get(&a.0), Some(&c.0));
        assert_eq!(merged.get(&b.0), Some(&c.0));

        let entry = |id: &EntryId| Entry {
            id: id.clone(),
            store: "s".into(),
            store_id: id.0.clone(),
            title_cache: None,
        };
        let mut snap = Snapshot {
            entries: vec![entry(&a), entry(&b), entry(&c)],
            impacts: vec![nrs_core::Impact {
                id: ImpactId("i1".into()),
                target: a.clone(),
                atoms: vec![atom(&a, "music", 1.0)],
            }],
            relations: vec![
                nrs_core::Relation {
                    kind: RelKind::Remix,
                    from: a.clone(),
                    to: c.clone(),
                    transform: Transform::Scalar(0.2),
                },
                nrs_core::Relation {
                    kind: RelKind::Contains,
                    from: b.clone(),
                    to: EntryId("other".into()),
                    transform: Transform::Scalar(1.0),
                },
            ],
            palette: nrs_core::Palette {
                anchors: vec![],
                k_feat: Default::default(),
            },
        };
        let mut extras = ImportExtras::default();
        extras.waivers.insert(a.clone(), ["r1".to_string()].into());
        extras.waivers.insert(c.clone(), ["r2".to_string()].into());
        store.follow_merges(&mut snap, &mut extras).unwrap();

        assert_eq!(snap.entries, vec![entry(&c)]);
        assert_eq!(snap.impacts[0].target, c);
        assert_eq!(snap.impacts[0].atoms[0].source.0, c.0);
        assert_eq!(
            snap.relations.len(),
            1,
            "a -> c became c -> c and is dropped"
        );
        assert_eq!(snap.relations[0].from, c);
        assert_eq!(
            extras.waivers.get(&c).unwrap().iter().collect::<Vec<_>>(),
            vec!["r1", "r2"]
        );
    }
}
