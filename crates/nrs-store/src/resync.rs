//! Re-applying a fresh import onto an already-populated store, without
//! losing what was done to it since (`nrs-import`'s `resync` binary).
//!
//! `import_snapshot` only works on an empty database, and impact ids are
//! assigned in parse order (`i1`, `i2`, ...), so a re-parse after an
//! importer fix renumbers everything after the first change. Matching is
//! therefore by *content*: an impact is its target, its atoms and its
//! DAH provenance; a relation is its kind, endpoints and transform.
//!
//! - Snapshot items with no stored counterpart are added (fresh ULIDs).
//! - Stored items with no snapshot counterpart are removed, unless they
//!   have been touched: any event naming them (edited, confirmed, added
//!   by hand) or, for impacts, a confirmed `note`. Touched items are
//!   always kept as they are.
//! - An item that was edited or deleted since the import must not come
//!   back as a duplicate, so its import-time state (the `before` of the
//!   earliest event naming it) is struck from the additions.
//! - Entries are only ever added, never removed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{params, Transaction};
use serde::Serialize;
use ulid::Ulid;

use nrs_core::{Atom, Entry, EntryId, Impact, ImpactId, RelKind, Relation, Snapshot, Transform};

use crate::crud::{append_event, to_json};
use crate::error::Result;
use crate::event::EventKind;
use crate::import::ImportExtras;
use crate::relkind::{rel_kind_from_str, rel_kind_to_str};
use crate::review::ReviewState;
use crate::Store;

/// What `apply_resync` would change. Built by `plan_resync`, so it can
/// be shown before anything is written.
#[derive(Debug, Default)]
pub struct ResyncPlan {
    pub entries_to_add: Vec<Entry>,
    /// Snapshot impacts, still carrying their snapshot ids (the keys
    /// into `ImportExtras`); they get fresh ids when applied.
    pub impacts_to_add: Vec<Impact>,
    pub impacts_to_remove: Vec<Impact>,
    /// Touched impacts with no snapshot counterpart, left alone.
    pub impacts_kept: Vec<Impact>,
    pub relations_to_add: Vec<Relation>,
    /// `(relation id, relation)`.
    pub relations_to_remove: Vec<(String, Relation)>,
    pub relations_kept: Vec<(String, Relation)>,
}

impl ResyncPlan {
    pub fn is_empty(&self) -> bool {
        self.entries_to_add.is_empty()
            && self.impacts_to_add.is_empty()
            && self.impacts_to_remove.is_empty()
            && self.relations_to_add.is_empty()
            && self.relations_to_remove.is_empty()
    }
}

#[derive(Serialize)]
struct ResyncSummary {
    entries_added: usize,
    impacts_added: usize,
    impacts_removed: usize,
    impacts_kept: usize,
    relations_added: usize,
    relations_removed: usize,
    relations_kept: usize,
}

/// An atom as a comparable value: blend and time as their canonical
/// JSON, magnitude by its exact `f32` bits (both sides come from the
/// same deterministic formulas, stored as `f32`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct AtomKey {
    source: String,
    blend: String,
    magnitude: u32,
    time: String,
}

fn atom_keys(atoms: &[Atom]) -> Result<Vec<AtomKey>> {
    let mut keys = atoms
        .iter()
        .map(|atom| {
            Ok(AtomKey {
                source: atom.source.0.clone(),
                blend: serde_json::to_string(&atom.blend)?,
                magnitude: atom.magnitude.to_bits(),
                time: serde_json::to_string(&atom.time)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    keys.sort();
    Ok(keys)
}

/// `(kind, params as canonical JSON, contributor factor bits)`.
type DahKey = Option<(String, String, u32)>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ImpactKey {
    target: String,
    atoms: Vec<AtomKey>,
    dah: DahKey,
}

/// Object keys sorted, so params written in a different key order (an
/// older importer) still compare equal.
fn canonical_params(json: &str) -> Result<String> {
    let map: BTreeMap<String, serde_json::Value> = serde_json::from_str(json)?;
    Ok(serde_json::to_string(&map)?)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RelationKey {
    kind: &'static str,
    from: String,
    to: String,
    transform: String,
}

fn relation_key(relation: &Relation) -> Result<RelationKey> {
    Ok(RelationKey {
        kind: rel_kind_to_str(relation.kind),
        from: relation.from.0.clone(),
        to: relation.to.0.clone(),
        transform: serde_json::to_string(&relation.transform)?,
    })
}

struct StoredImpact {
    impact: Impact,
    dah: DahKey,
    noted: bool,
}

/// Per stored-or-deleted item id: whether any event names it, and the
/// `before` of the earliest event that has one (its import-time state).
#[derive(Default)]
struct History {
    touched: BTreeSet<String>,
    original: BTreeMap<String, String>,
    deleted: BTreeSet<String>,
}

fn now_ts() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

impl Store {
    pub fn plan_resync(&self, snap: &Snapshot, extras: &ImportExtras) -> Result<ResyncPlan> {
        let mut plan = ResyncPlan::default();

        let stored_entries: BTreeSet<String> =
            self.list_entries()?.into_iter().map(|e| e.id.0).collect();
        let mut seen = BTreeSet::new();
        for entry in &snap.entries {
            if !stored_entries.contains(&entry.id.0) && seen.insert(entry.id.0.clone()) {
                plan.entries_to_add.push(entry.clone());
            }
        }

        let impact_history = self.history("impact.")?;
        let relation_history = self.history("relation.")?;

        // --- impacts
        let stored = self.stored_impacts()?;
        let mut by_key: HashMap<ImpactKey, Vec<usize>> = HashMap::new();
        for (i, s) in stored.iter().enumerate().rev() {
            let key = ImpactKey {
                target: s.impact.target.0.clone(),
                atoms: atom_keys(&s.impact.atoms)?,
                dah: s.dah.clone(),
            };
            by_key.entry(key).or_default().push(i);
        }
        let mut matched = vec![false; stored.len()];
        for impact in &snap.impacts {
            let dah = extras
                .impact_dah
                .get(&impact.id)
                .map(|(kind, params, factor)| {
                    Ok::<_, crate::error::StoreError>((
                        kind.clone(),
                        serde_json::to_string(params)?,
                        factor.to_bits(),
                    ))
                })
                .transpose()?;
            let key = ImpactKey {
                target: impact.target.0.clone(),
                atoms: atom_keys(&impact.atoms)?,
                dah,
            };
            match by_key.get_mut(&key).and_then(Vec::pop) {
                Some(i) => matched[i] = true,
                None => plan.impacts_to_add.push(impact.clone()),
            }
        }
        let mut superseded: Vec<String> = impact_history.deleted.iter().cloned().collect();
        for (s, matched) in stored.into_iter().zip(matched) {
            if matched {
                continue;
            }
            if s.noted || impact_history.touched.contains(&s.impact.id.0) {
                superseded.push(s.impact.id.0.clone());
                plan.impacts_kept.push(s.impact);
            } else {
                plan.impacts_to_remove.push(s.impact);
            }
        }
        for id in superseded {
            let Some(before) = impact_history.original.get(&id) else {
                continue;
            };
            let original: Impact = serde_json::from_str(before)?;
            let original_atoms = atom_keys(&original.atoms)?;
            let mut position = None;
            for (i, candidate) in plan.impacts_to_add.iter().enumerate() {
                if candidate.target == original.target
                    && atom_keys(&candidate.atoms)? == original_atoms
                {
                    position = Some(i);
                    break;
                }
            }
            if let Some(i) = position {
                plan.impacts_to_add.remove(i);
            }
        }

        // --- relations
        let stored = self.stored_relations()?;
        let mut by_key: HashMap<RelationKey, Vec<usize>> = HashMap::new();
        for (i, (_, relation)) in stored.iter().enumerate().rev() {
            by_key.entry(relation_key(relation)?).or_default().push(i);
        }
        let mut matched = vec![false; stored.len()];
        for relation in &snap.relations {
            match by_key.get_mut(&relation_key(relation)?).and_then(Vec::pop) {
                Some(i) => matched[i] = true,
                None => plan.relations_to_add.push(relation.clone()),
            }
        }
        let mut superseded: Vec<String> = relation_history.deleted.iter().cloned().collect();
        for ((id, relation), matched) in stored.into_iter().zip(matched) {
            if matched {
                continue;
            }
            if relation_history.touched.contains(&id) {
                superseded.push(id.clone());
                plan.relations_kept.push((id, relation));
            } else {
                plan.relations_to_remove.push((id, relation));
            }
        }
        for id in superseded {
            let Some(before) = relation_history.original.get(&id) else {
                continue;
            };
            let original = relation_key(&serde_json::from_str::<Relation>(before)?)?;
            let mut position = None;
            for (i, candidate) in plan.relations_to_add.iter().enumerate() {
                if relation_key(candidate)? == original {
                    position = Some(i);
                    break;
                }
            }
            if let Some(i) = position {
                plan.relations_to_add.remove(i);
            }
        }

        Ok(plan)
    }

    /// Writes `plan` (from `plan_resync` over the same `snap`/`extras`)
    /// as one transaction, one event per change plus a closing
    /// `snapshot.resync` summary, all carrying `note`. Then updates the
    /// review sidecar: new impacts get their import comment/flag, removed
    /// ones lose theirs.
    pub fn apply_resync(
        &mut self,
        plan: &ResyncPlan,
        extras: &ImportExtras,
        note: &str,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let note = Some(note);

        let mut claimed: BTreeSet<(String, String)> = {
            let mut stmt = tx.prepare("SELECT store, store_id FROM entry")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for entry in &plan.entries_to_add {
            // Same `(store, store_id)` collision fallback as
            // `import_snapshot`.
            let (store, store_id) = if claimed.insert((entry.store.clone(), entry.store_id.clone()))
            {
                (entry.store.as_str(), entry.store_id.as_str())
            } else {
                ("none", entry.id.0.as_str())
            };
            tx.execute(
                "INSERT INTO entry (id, store, store_id, title_cache) VALUES (?1, ?2, ?3, ?4)",
                params![entry.id.0, store, store_id, entry.title_cache],
            )?;
            append_event(
                &tx,
                EventKind::EntryAdd,
                Some(&entry.id.0),
                None,
                to_json(Some(entry))?.as_deref(),
                note,
            )?;
        }

        for impact in &plan.impacts_to_remove {
            tx.execute(
                "DELETE FROM impact_atom WHERE impact = ?1",
                params![impact.id.0],
            )?;
            tx.execute("DELETE FROM impact WHERE id = ?1", params![impact.id.0])?;
            append_event(
                &tx,
                EventKind::ImpactDelete,
                Some(&impact.id.0),
                to_json(Some(impact))?.as_deref(),
                None,
                note,
            )?;
        }

        let mut review_states = Vec::new();
        for impact in &plan.impacts_to_add {
            let id = Ulid::new().to_string();
            insert_impact(&tx, &id, impact, extras)?;
            let stored = Impact {
                id: ImpactId(id.clone()),
                ..impact.clone()
            };
            append_event(
                &tx,
                EventKind::ImpactAdd,
                Some(&id),
                None,
                to_json(Some(&stored))?.as_deref(),
                note,
            )?;
            let source_comment = extras.comments.get(&impact.id).cloned();
            let needs_review = extras.needs_review.contains(&impact.id);
            if source_comment.is_some() || needs_review {
                review_states.push((
                    id,
                    ReviewState {
                        source_comment,
                        needs_review,
                    },
                ));
            }
        }

        for (id, relation) in &plan.relations_to_remove {
            tx.execute("DELETE FROM relation WHERE id = ?1", params![id])?;
            append_event(
                &tx,
                EventKind::RelationDelete,
                Some(id),
                to_json(Some(relation))?.as_deref(),
                None,
                note,
            )?;
        }

        for relation in &plan.relations_to_add {
            let id = Ulid::new().to_string();
            let dah_params = if relation.kind == RelKind::Credit {
                extras
                    .credit_roles
                    .get(&(relation.from.clone(), relation.to.clone()))
                    .map(|roles| serde_json::json!({ "roles": roles }).to_string())
            } else {
                None
            };
            tx.execute(
                "INSERT INTO relation (id, kind, from_e, to_e, transform, dah_params) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    rel_kind_to_str(relation.kind),
                    relation.from.0,
                    relation.to.0,
                    serde_json::to_string(&relation.transform)?,
                    dah_params
                ],
            )?;
            append_event(
                &tx,
                EventKind::RelationAdd,
                Some(&id),
                None,
                to_json(Some(relation))?.as_deref(),
                note,
            )?;
        }

        let summary = serde_json::to_string(&ResyncSummary {
            entries_added: plan.entries_to_add.len(),
            impacts_added: plan.impacts_to_add.len(),
            impacts_removed: plan.impacts_to_remove.len(),
            impacts_kept: plan.impacts_kept.len(),
            relations_added: plan.relations_to_add.len(),
            relations_removed: plan.relations_to_remove.len(),
            relations_kept: plan.relations_kept.len(),
        })?;
        append_event(
            &tx,
            EventKind::SnapshotResync,
            None,
            None,
            Some(&summary),
            note,
        )?;
        tx.commit()?;

        self.review.seed_bulk(review_states)?;
        self.review
            .remove_bulk(plan.impacts_to_remove.iter().map(|i| i.id.0.as_str()))?;
        Ok(())
    }

    /// The date of the most recent `snapshot.import`, i.e. the `today`
    /// the stored impacts' date-dependent magnitudes were computed for.
    pub fn last_import_date(&self) -> Result<Option<time::Date>> {
        let ts: Option<i64> = self.conn.query_row(
            "SELECT MAX(ts) FROM event WHERE kind = ?1",
            params![EventKind::SnapshotImport.as_str()],
            |row| row.get(0),
        )?;
        Ok(ts
            .and_then(|ts| time::OffsetDateTime::from_unix_timestamp(ts).ok())
            .map(|t| t.date()))
    }

    fn history(&self, kind_prefix: &str) -> Result<History> {
        let mut history = History::default();
        let mut stmt = self.conn.prepare(
            "SELECT kind, target, before FROM event \
             WHERE kind LIKE ?1 AND target IS NOT NULL ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![format!("{kind_prefix}%")], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (kind, target, before) = row?;
            if kind.ends_with(".delete") {
                history.deleted.insert(target.clone());
            }
            if let Some(before) = before {
                history.original.entry(target.clone()).or_insert(before);
            }
            history.touched.insert(target);
        }
        Ok(history)
    }

    fn stored_impacts(&self) -> Result<Vec<StoredImpact>> {
        let mut atoms: BTreeMap<String, Vec<Atom>> = BTreeMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT impact, source, blend, magnitude, time FROM impact_atom ORDER BY rowid",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f32>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            for row in rows {
                let (impact, source, blend, magnitude, time) = row?;
                atoms.entry(impact).or_default().push(Atom {
                    source: nrs_core::SourceId(source),
                    blend: serde_json::from_str(&blend)?,
                    magnitude,
                    time: serde_json::from_str(&time)?,
                });
            }
        }

        let mut stmt = self.conn.prepare(
            "SELECT id, target, note, dah_kind, dah_params, dah_factor FROM impact ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<f32>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, target, note, dah_kind, dah_params, dah_factor) = row?;
            let dah = match (dah_kind, dah_params, dah_factor) {
                (Some(kind), Some(params), Some(factor)) => {
                    Some((kind, canonical_params(&params)?, factor.to_bits()))
                }
                _ => None,
            };
            out.push(StoredImpact {
                impact: Impact {
                    atoms: atoms.remove(&id).unwrap_or_default(),
                    id: ImpactId(id),
                    target: EntryId(target),
                },
                dah,
                noted: note.is_some(),
            });
        }
        Ok(out)
    }

    fn stored_relations(&self) -> Result<Vec<(String, Relation)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, from_e, to_e, transform FROM relation ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, from, to, transform) = row?;
            let transform: Transform = serde_json::from_str(&transform)?;
            out.push((
                id,
                Relation {
                    kind: rel_kind_from_str(&kind)?,
                    from: EntryId(from),
                    to: EntryId(to),
                    transform,
                },
            ));
        }
        Ok(out)
    }
}

fn insert_impact(tx: &Transaction, id: &str, impact: &Impact, extras: &ImportExtras) -> Result<()> {
    let (dah_kind, dah_params, dah_factor) = match extras.impact_dah.get(&impact.id) {
        Some((kind, params, factor)) => (
            Some(kind.as_str()),
            Some(serde_json::to_string(params)?),
            Some(*factor),
        ),
        None => (None, None, None),
    };
    tx.execute(
        "INSERT INTO impact (id, target, note, created, dah_kind, dah_params, dah_factor) \
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
        params![
            id,
            impact.target.0,
            now_ts(),
            dah_kind,
            dah_params,
            dah_factor
        ],
    )?;
    for atom in &impact.atoms {
        tx.execute(
            "INSERT INTO impact_atom (impact, source, blend, magnitude, time) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                atom.source.0,
                serde_json::to_string(&atom.blend)?,
                atom.magnitude,
                serde_json::to_string(&atom.time)?
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nrs_core::{
        AnchorId, Atom, Entry, EntryId, Impact, ImpactId, Palette, RelKind, Relation, Snapshot,
        SourceId, TimeSupport, Transform,
    };

    use crate::{ImportExtras, Store};

    fn entry(id: &str) -> Entry {
        Entry {
            id: EntryId(id.into()),
            store: "none".into(),
            store_id: id.into(),
            title_cache: None,
        }
    }

    fn impact(id: &str, target: &str, anchor: &str, magnitude: f32) -> Impact {
        Impact {
            id: ImpactId(id.into()),
            target: EntryId(target.into()),
            atoms: vec![Atom {
                source: SourceId(target.into()),
                blend: vec![(AnchorId(anchor.into()), 1.0)],
                magnitude,
                time: TimeSupport::Moment,
            }],
        }
    }

    fn remix(from: &str, to: &str, w: f32) -> Relation {
        Relation {
            kind: RelKind::Remix,
            from: EntryId(from.into()),
            to: EntryId(to.into()),
            transform: Transform::Scalar(w),
        }
    }

    fn snapshot(impacts: Vec<Impact>, relations: Vec<Relation>) -> Snapshot {
        Snapshot {
            entries: vec![entry("A"), entry("B")],
            impacts,
            relations,
            palette: Palette {
                anchors: vec![],
                k_feat: Default::default(),
            },
        }
    }

    fn impact_atoms(store: &Store, target: &str) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = store
            .impacts_for_entry(&EntryId(target.into()))
            .unwrap()
            .into_iter()
            .flat_map(|i| i.atoms)
            .map(|a| (a.blend[0].0 .0.clone(), a.magnitude))
            .collect();
        out.sort_by(|a, b| a.partial_cmp(b).unwrap());
        out
    }

    #[test]
    fn resync_adds_removes_and_respects_edits() {
        let mut store = Store::open_in_memory().unwrap();
        let old = snapshot(
            vec![
                impact("i1", "A", "comedy", 1.0),
                impact("i2", "A", "tragedy", 2.0), // stale: re-import gives 0.5
                impact("i3", "B", "hype", 3.0),    // edited by hand
                impact("i4", "B", "cozy", 4.0),    // deleted by hand
            ],
            vec![remix("A", "B", 0.2)],
        );
        store
            .import_snapshot(&old, &ImportExtras::default())
            .unwrap();
        store
            .edit_impact(
                &ImpactId("i3".into()),
                &impact("x", "B", "meme", 3.0).atoms,
                None,
            )
            .unwrap();
        store.delete_impact(&ImpactId("i4".into())).unwrap();
        let hand_made = store
            .add_impact(
                &EntryId("A".into()),
                &impact("x", "A", "waifu", 9.0).atoms,
                None,
            )
            .unwrap();

        // The re-import renumbers everything, fixes i2, adds a new
        // impact, and still contains the originals of i3 and i4.
        let new = snapshot(
            vec![
                impact("i1", "B", "visual", 5.0),
                impact("i2", "A", "comedy", 1.0),
                impact("i3", "A", "tragedy", 0.5),
                impact("i4", "B", "hype", 3.0),
                impact("i5", "B", "cozy", 4.0),
            ],
            vec![remix("A", "B", 0.1), remix("B", "A", 0.2)],
        );
        let plan = store.plan_resync(&new, &ImportExtras::default()).unwrap();
        assert_eq!(plan.impacts_to_remove.len(), 1);
        assert_eq!(plan.impacts_to_remove[0].id, ImpactId("i2".into()));
        let added: Vec<_> = plan
            .impacts_to_add
            .iter()
            .map(|i| i.id.0.as_str())
            .collect();
        assert_eq!(added, ["i1", "i3"]);
        let kept: Vec<_> = plan.impacts_kept.iter().map(|i| i.id.clone()).collect();
        assert_eq!(kept, [ImpactId("i3".into()), hand_made]);
        assert_eq!(plan.relations_to_add.len(), 2);
        assert_eq!(plan.relations_to_remove.len(), 1);

        store
            .apply_resync(&plan, &ImportExtras::default(), "test")
            .unwrap();
        assert_eq!(
            impact_atoms(&store, "A"),
            [
                ("comedy".into(), 1.0),
                ("tragedy".into(), 0.5),
                ("waifu".into(), 9.0)
            ]
        );
        assert_eq!(
            impact_atoms(&store, "B"),
            [("meme".into(), 3.0), ("visual".into(), 5.0)]
        );

        // Applying is idempotent: a second plan has nothing to do.
        let again = store.plan_resync(&new, &ImportExtras::default()).unwrap();
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn dah_params_key_order_does_not_matter() {
        let mut store = Store::open_in_memory().unwrap();
        let snap = snapshot(vec![impact("i1", "A", "comedy", 1.0)], vec![]);
        let mut params = BTreeMap::new();
        params.insert("b".to_string(), "2".to_string());
        params.insert("a".to_string(), "1".to_string());
        let mut extras = ImportExtras::default();
        extras
            .impact_dah
            .insert(ImpactId("i1".into()), ("nei".into(), params, 1.0));
        store.import_snapshot(&snap, &extras).unwrap();
        store
            .conn
            .execute(
                "UPDATE impact SET dah_params = '{\"b\":\"2\",\"a\":\"1\"}'",
                [],
            )
            .unwrap();
        assert!(store.plan_resync(&snap, &extras).unwrap().is_empty());
    }
}
