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
    /// Existing entries whose snapshot title differs from the stored
    /// `title_cache` (stripped XML takes titles from the stores).
    pub entries_to_retitle: Vec<RetitledEntry>,
    /// Existing entries whose stripped-XML `<bind url>` differs from the
    /// stored `(store, store_id)`: they become `("url", <bind URL>)`.
    pub entries_to_rebind: Vec<ReboundEntry>,
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
    /// Shared impacts to create, extend, or fold together from the
    /// per-contributor rows older databases stored (schema < V8).
    pub shared_changes: Vec<SharedChange>,
}

/// One shared impact's change, keyed by the import's share key.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedChange {
    pub key: String,
    pub base: SharedBase,
    /// Stored per-contributor rows that become members: `(row id,
    /// contributor, share)`. For `SharedBase::Fold` the first row is
    /// converted into the shared impact itself.
    pub fold: Vec<(ImpactId, EntryId, f32)>,
    /// Snapshot impacts that become new members.
    pub add: Vec<Impact>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SharedBase {
    /// Already stored as a shared impact with this id.
    Existing(String),
    /// Reuse this per-contributor row's id for the shared impact.
    Fold(String),
    /// A new shared impact.
    New,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetitledEntry {
    pub id: EntryId,
    pub before: Option<String>,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReboundEntry {
    pub id: EntryId,
    pub before: (String, String),
    pub after: (String, String),
}

impl ResyncPlan {
    pub fn is_empty(&self) -> bool {
        self.entries_to_add.is_empty()
            && self.entries_to_retitle.is_empty()
            && self.entries_to_rebind.is_empty()
            && self.shared_changes.is_empty()
            && self.impacts_to_add.is_empty()
            && self.impacts_to_remove.is_empty()
            && self.relations_to_add.is_empty()
            && self.relations_to_remove.is_empty()
    }
}

#[derive(Serialize)]
struct ResyncSummary {
    entries_added: usize,
    entries_retitled: usize,
    impacts_added: usize,
    impacts_removed: usize,
    impacts_kept: usize,
    relations_added: usize,
    relations_removed: usize,
    relations_kept: usize,
    shared_changed: usize,
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

/// `atom_keys` without `time`, for recognising an edited impact's
/// import-time original: dating impacts (`TimeSupport::Interval`) came
/// after some edits, so an event's `before` can hold the undated form
/// of what the importer now emits dated.
fn untimed_atom_keys(atoms: &[Atom]) -> Result<Vec<AtomKey>> {
    let mut keys: Vec<AtomKey> = atom_keys(atoms)?
        .into_iter()
        .map(|key| AtomKey {
            time: String::new(),
            ..key
        })
        .collect();
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

/// One import share key while planning: the stored shared impacts its
/// members matched, the per-contributor rows to fold in, and the snapshot
/// impacts with no stored counterpart.
#[derive(Default)]
struct ShareGroup {
    bases: BTreeSet<String>,
    fold: Vec<(ImpactId, EntryId, f32)>,
    add: Vec<Impact>,
}

struct StoredImpact {
    /// An ordinary impact, or one member's expanded copy of a shared one.
    impact: Impact,
    dah: DahKey,
    noted: bool,
    /// The shared impact this copy expands, if any.
    base: Option<String>,
}

/// Every event `apply_resync` writes has a note starting with this, so a
/// later plan can tell the import's own changes from hand changes.
pub const RESYNC_NOTE_PREFIX: &str = "resync:";

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

        let stored_entries: BTreeMap<String, Entry> = self
            .list_entries()?
            .into_iter()
            .map(|e| (e.id.0.clone(), e))
            .collect();
        let mut seen = BTreeSet::new();
        for entry in &snap.entries {
            if !seen.insert(entry.id.0.clone()) {
                continue;
            }
            match stored_entries.get(&entry.id.0) {
                None => plan.entries_to_add.push(entry.clone()),
                // A snapshot with no title (an unbound entry the stores
                // couldn't name) never blanks a stored one.
                Some(stored) => {
                    if let Some(title) = &entry.title_cache {
                        if stored.title_cache.as_deref() != Some(title.as_str()) {
                            plan.entries_to_retitle.push(RetitledEntry {
                                id: entry.id.clone(),
                                before: stored.title_cache.clone(),
                                after: title.clone(),
                            });
                        }
                    }
                    // Only a `<bind url>` (store "url") rebinds: legacy XML
                    // ids are a best-effort guess that never overwrites.
                    if entry.store == "url"
                        && (stored.store.as_str(), stored.store_id.as_str())
                            != (entry.store.as_str(), entry.store_id.as_str())
                    {
                        plan.entries_to_rebind.push(ReboundEntry {
                            id: entry.id.clone(),
                            before: (stored.store.clone(), stored.store_id.clone()),
                            after: (entry.store.clone(), entry.store_id.clone()),
                        });
                    }
                }
            }
        }

        // Two entries binding the same URL: the first keeps it.
        let mut taken: BTreeSet<(String, String)> = stored_entries
            .values()
            .map(|e| (e.store.clone(), e.store_id.clone()))
            .collect();
        for rebind in &plan.entries_to_rebind {
            taken.remove(&rebind.before);
        }
        plan.entries_to_rebind.retain(|rebind| taken.insert(rebind.after.clone()));

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
        let mut matched_to: HashMap<ImpactId, usize> = HashMap::new();
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
                Some(i) => {
                    matched[i] = true;
                    matched_to.insert(impact.id.clone(), i);
                }
                None => plan.impacts_to_add.push(impact.clone()),
            }
        }
        let mut superseded: BTreeSet<String> = impact_history.deleted.iter().cloned().collect();
        for (s, matched) in stored.iter().zip(&matched) {
            if *matched {
                continue;
            }
            // A member's copy counts as touched when its shared impact is.
            let touched = impact_history.touched.contains(&s.impact.id.0)
                || s.base
                    .as_ref()
                    .is_some_and(|b| impact_history.touched.contains(b));
            if s.noted || touched {
                superseded.insert(s.base.clone().unwrap_or_else(|| s.impact.id.0.clone()));
                plan.impacts_kept.push(s.impact.clone());
            } else {
                plan.impacts_to_remove.push(s.impact.clone());
            }
        }
        for id in superseded {
            let Some(before) = impact_history.original.get(&id) else {
                continue;
            };
            // A shared impact's original strikes each member's copy.
            let original: crate::shared::ImpactRecord = serde_json::from_str(before)?;
            for original in original.expanded() {
                let original_atoms = untimed_atom_keys(&original.atoms)?;
                let mut position = None;
                for (i, candidate) in plan.impacts_to_add.iter().enumerate() {
                    if candidate.target == original.target
                        && untimed_atom_keys(&candidate.atoms)? == original_atoms
                    {
                        position = Some(i);
                        break;
                    }
                }
                if let Some(i) = position {
                    plan.impacts_to_add.remove(i);
                }
            }
        }

        // --- shared impacts: group the import's per-contributor shares.
        let mut by_share_key: BTreeMap<&str, ShareGroup> = BTreeMap::new();
        for impact in &snap.impacts {
            let Some((key, factor)) = extras.impact_share.get(&impact.id) else {
                continue;
            };
            let slot = by_share_key.entry(key.as_str()).or_default();
            match matched_to.get(&impact.id).map(|&i| &stored[i]) {
                Some(s) => match &s.base {
                    Some(base) => {
                        slot.bases.insert(base.clone());
                    }
                    None => slot
                        .fold
                        .push((s.impact.id.clone(), s.impact.target.clone(), *factor)),
                },
                None => {
                    if let Some(i) = plan.impacts_to_add.iter().position(|a| a.id == impact.id) {
                        slot.add.push(plan.impacts_to_add.remove(i));
                    }
                }
            }
        }
        for (key, ShareGroup { bases, fold, add }) in by_share_key {
            if fold.is_empty() && add.is_empty() {
                continue;
            }
            let base = match bases.into_iter().next() {
                Some(existing) => SharedBase::Existing(existing),
                None => match fold.first() {
                    Some((first, _, _)) => SharedBase::Fold(first.0.clone()),
                    None => SharedBase::New,
                },
            };
            plan.shared_changes.push(SharedChange {
                key: key.to_string(),
                base,
                fold,
                add,
            });
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
        let note = if note.starts_with(RESYNC_NOTE_PREFIX) {
            note.to_string()
        } else {
            format!("{RESYNC_NOTE_PREFIX} {note}")
        };
        let note = Some(note.as_str());

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

        for retitle in &plan.entries_to_retitle {
            tx.execute(
                "UPDATE entry SET title_cache = ?2 WHERE id = ?1",
                params![retitle.id.0, retitle.after],
            )?;
            append_event(
                &tx,
                EventKind::EntryEdit,
                Some(&retitle.id.0),
                Some(&serde_json::json!({ "title_cache": retitle.before }).to_string()),
                Some(&serde_json::json!({ "title_cache": retitle.after }).to_string()),
                note,
            )?;
        }

        // Free every old binding first, so swapped bindings don't collide.
        for rebind in &plan.entries_to_rebind {
            tx.execute(
                "UPDATE entry SET store = 'none', store_id = id WHERE id = ?1",
                params![rebind.id.0],
            )?;
        }
        for rebind in &plan.entries_to_rebind {
            tx.execute(
                "UPDATE entry SET store = ?2, store_id = ?3 WHERE id = ?1",
                params![rebind.id.0, rebind.after.0, rebind.after.1],
            )?;
            append_event(
                &tx,
                EventKind::EntryEdit,
                Some(&rebind.id.0),
                Some(&serde_json::json!({ "store": rebind.before.0, "store_id": rebind.before.1 }).to_string()),
                Some(&serde_json::json!({ "store": rebind.after.0, "store_id": rebind.after.1 }).to_string()),
                note,
            )?;
        }

        for impact in &plan.impacts_to_remove {
            if let Some((base, contributor)) = crate::shared::split_member_impact_id(&impact.id.0) {
                tx.execute(
                    "DELETE FROM impact_member WHERE impact = ?1 AND contributor = ?2",
                    params![base, contributor],
                )?;
                if crate::shared::members_of(&tx, base)?.is_empty() {
                    tx.execute("DELETE FROM impact_split WHERE impact = ?1", params![base])?;
                    tx.execute("DELETE FROM impact_atom WHERE impact = ?1", params![base])?;
                    tx.execute("DELETE FROM impact WHERE id = ?1", params![base])?;
                }
                append_event(
                    &tx,
                    EventKind::ImpactDelete,
                    Some(&impact.id.0),
                    to_json(Some(impact))?.as_deref(),
                    None,
                    note,
                )?;
                continue;
            }
            tx.execute(
                "DELETE FROM impact_atom WHERE impact = ?1",
                params![impact.id.0],
            )?;
            tx.execute(
                "DELETE FROM impact_member WHERE impact = ?1",
                params![impact.id.0],
            )?;
            tx.execute(
                "DELETE FROM impact_split WHERE impact = ?1",
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

        let known: BTreeSet<String> = {
            let mut stmt = tx.prepare("SELECT id FROM entry")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut folded_away: Vec<String> = Vec::new();
        for change in &plan.shared_changes {
            apply_shared_change(
                &tx,
                change,
                extras,
                &known,
                note,
                &mut review_states,
                &mut folded_away,
            )?;
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
            entries_retitled: plan.entries_to_retitle.len(),
            impacts_added: plan.impacts_to_add.len(),
            impacts_removed: plan.impacts_to_remove.len(),
            impacts_kept: plan.impacts_kept.len(),
            relations_added: plan.relations_to_add.len(),
            relations_removed: plan.relations_to_remove.len(),
            relations_kept: plan.relations_kept.len(),
            shared_changed: plan.shared_changes.len(),
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
        self.review.remove_bulk(
            plan.impacts_to_remove
                .iter()
                .map(|i| i.id.0.as_str())
                .chain(folded_away.iter().map(String::as_str)),
        )?;
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
        // Resync's own events are the import, not a hand change: counting
        // them would pin every impact a resync added (or strike every one
        // it removed) against all later imports.
        let mut stmt = self.conn.prepare(
            "SELECT kind, target, before FROM event \
             WHERE kind LIKE ?1 AND target IS NOT NULL \
             AND (note IS NULL OR substr(note, 1, length(?2)) != ?2) ORDER BY seq",
        )?;
        let rows = stmt.query_map(
            params![format!("{kind_prefix}%"), RESYNC_NOTE_PREFIX],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
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
        let mut members = crate::shared::load_all_members(&self.conn)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, target, note, dah_kind, dah_params, dah_factor) = row?;
            let impact = Impact {
                atoms: atoms.remove(&id).unwrap_or_default(),
                id: ImpactId(id),
                target: EntryId(target),
            };
            let Some(members) = members.remove(&impact.id.0) else {
                let dah = match (dah_kind, dah_params, dah_factor) {
                    (Some(kind), Some(params), Some(factor)) => {
                        Some((kind, canonical_params(&params)?, factor.to_bits()))
                    }
                    _ => None,
                };
                out.push(StoredImpact {
                    impact,
                    dah,
                    noted: note.is_some(),
                    base: None,
                });
                continue;
            };
            // Each member's copy carries its own share as the DAH factor,
            // the way the importer records a contributor's share.
            let params = dah_params.as_deref().map(canonical_params).transpose()?;
            for (copy, member) in crate::shared::expand(&impact, &members)
                .into_iter()
                .zip(&members)
            {
                let dah = match (&dah_kind, &params) {
                    (Some(kind), Some(params)) => {
                        Some((kind.clone(), params.clone(), member.factor.to_bits()))
                    }
                    _ => None,
                };
                out.push(StoredImpact {
                    impact: copy,
                    dah,
                    noted: note.is_some(),
                    base: Some(impact.id.0.clone()),
                });
            }
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

/// Writes one `SharedChange`: creates the shared impact (or converts the
/// first folded row into it), adds every member, and drops the other
/// folded rows. One `impact.add`/`impact.edit` event for the shared
/// impact, one `impact.delete` per row folded away.
fn apply_shared_change(
    tx: &Transaction,
    change: &SharedChange,
    extras: &ImportExtras,
    known: &BTreeSet<String>,
    note: Option<&str>,
    review_states: &mut Vec<(String, ReviewState)>,
    folded_away: &mut Vec<String>,
) -> Result<()> {
    let base_atoms = extras.shared_base.get(&change.key).ok_or_else(|| {
        crate::error::StoreError::InvalidData(format!(
            "no base atoms for shared impact {}",
            change.key
        ))
    })?;
    // DAH provenance comes from any one member: all share kind and params.
    let sample = change
        .add
        .first()
        .map(|impact| impact.id.clone())
        .or_else(|| {
            extras
                .impact_share
                .iter()
                .find(|(_, (key, _))| *key == change.key)
                .map(|(id, _)| id.clone())
        });
    let dah = sample.as_ref().and_then(|id| extras.impact_dah.get(id));
    let first_member = change
        .fold
        .first()
        .map(|(_, contributor, _)| contributor.clone())
        .or_else(|| change.add.first().map(|impact| impact.target.clone()))
        .ok_or_else(|| {
            crate::error::StoreError::InvalidData(format!("empty shared change {}", change.key))
        })?;
    let home = crate::import::shared_home(base_atoms, &first_member, known);

    let write_base = |id: &str| -> Result<()> {
        tx.execute("DELETE FROM impact_atom WHERE impact = ?1", params![id])?;
        for atom in base_atoms {
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
    };

    let (id, before) = match &change.base {
        SharedBase::Existing(id) => (
            id.clone(),
            crate::shared::read_record(tx, &ImpactId(id.clone()))?,
        ),
        SharedBase::Fold(id) => {
            let before = crate::shared::read_record(tx, &ImpactId(id.clone()))?;
            tx.execute(
                "UPDATE impact SET target = ?2, dah_factor = CASE WHEN dah_kind IS NULL THEN NULL ELSE 1.0 END \
                 WHERE id = ?1",
                params![id, home.0],
            )?;
            write_base(id)?;
            (id.clone(), before)
        }
        SharedBase::New => {
            let id = Ulid::new().to_string();
            let (dah_kind, dah_params) = match dah {
                Some((kind, params, _)) => {
                    (Some(kind.as_str()), Some(serde_json::to_string(params)?))
                }
                None => (None, None),
            };
            tx.execute(
                "INSERT INTO impact (id, target, note, created, dah_kind, dah_params, dah_factor) \
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    home.0,
                    now_ts(),
                    dah_kind,
                    dah_params,
                    dah_kind.map(|_| 1.0f32)
                ],
            )?;
            write_base(&id)?;
            if let Some(sample) = &sample {
                let source_comment = extras.comments.get(sample).cloned();
                let needs_review = extras.needs_review.contains(sample);
                if source_comment.is_some() || needs_review {
                    review_states.push((
                        id.clone(),
                        ReviewState {
                            source_comment,
                            needs_review,
                        },
                    ));
                }
            }
            (id, None)
        }
    };

    let mut members = Vec::new();
    for (row, contributor, factor) in &change.fold {
        if row.0 != id {
            let row_before = crate::shared::read_record(tx, row)?;
            tx.execute("DELETE FROM impact_atom WHERE impact = ?1", params![row.0])?;
            tx.execute("DELETE FROM impact WHERE id = ?1", params![row.0])?;
            append_event(
                tx,
                EventKind::ImpactDelete,
                Some(&row.0),
                to_json(row_before.as_ref())?.as_deref(),
                None,
                note,
            )?;
            folded_away.push(row.0.clone());
        }
        members.push(crate::shared::ImpactMember {
            contributor: contributor.clone(),
            factor: *factor,
        });
    }
    for impact in &change.add {
        let factor = extras.impact_share.get(&impact.id).map_or(1.0, |(_, f)| *f);
        members.push(crate::shared::ImpactMember {
            contributor: impact.target.clone(),
            factor,
        });
    }
    crate::shared::insert_members(tx, &id, &members)?;

    let after = crate::shared::read_record(tx, &ImpactId(id.clone()))?;
    append_event(
        tx,
        if before.is_some() {
            EventKind::ImpactEdit
        } else {
            EventKind::ImpactAdd
        },
        Some(&id),
        to_json(before.as_ref())?.as_deref(),
        to_json(after.as_ref())?.as_deref(),
        note,
    )?;
    Ok(())
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
    fn edited_impact_stays_struck_once_the_import_dates_it() {
        let mut store = Store::open_in_memory().unwrap();
        let old = snapshot(vec![impact("i1", "A", "meme", 5.0)], vec![]);
        store
            .import_snapshot(&old, &ImportExtras::default())
            .unwrap();
        store
            .edit_impact(
                &ImpactId("i1".into()),
                &impact("x", "A", "meme", 3.0).atoms,
                None,
            )
            .unwrap();

        // The importer now emits the same impact over a period.
        let mut dated = impact("i1", "A", "meme", 5.0);
        dated.atoms[0].time = TimeSupport::Interval {
            from: 20_000,
            to: 20_163,
        };
        let plan = store
            .plan_resync(&snapshot(vec![dated], vec![]), &ImportExtras::default())
            .unwrap();
        assert!(plan.impacts_to_add.is_empty(), "{plan:?}");
        assert_eq!(plan.impacts_kept.len(), 1);
    }

    #[test]
    fn resync_added_impacts_follow_later_imports() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .import_snapshot(&snapshot(vec![], vec![]), &ImportExtras::default())
            .unwrap();

        // A first resync adds an impact; a second import changes it.
        let first = snapshot(vec![impact("i1", "A", "music", 4.0)], vec![]);
        let plan = store.plan_resync(&first, &ImportExtras::default()).unwrap();
        store
            .apply_resync(&plan, &ImportExtras::default(), "test")
            .unwrap();
        let second = snapshot(vec![impact("i1", "A", "music", 4.7)], vec![]);
        let plan = store
            .plan_resync(&second, &ImportExtras::default())
            .unwrap();
        assert_eq!(plan.impacts_to_remove.len(), 1, "{plan:?}");
        assert!(plan.impacts_kept.is_empty(), "{plan:?}");
        assert_eq!(plan.impacts_to_add.len(), 1);
        store
            .apply_resync(&plan, &ImportExtras::default(), "test")
            .unwrap();
        assert_eq!(impact_atoms(&store, "A"), [("music".into(), 4.7)]);

        // Going back to the first import re-adds what the resync removed.
        let plan = store.plan_resync(&first, &ImportExtras::default()).unwrap();
        assert_eq!(plan.impacts_to_add.len(), 1, "{plan:?}");
        assert_eq!(plan.impacts_to_remove.len(), 1);
    }

    #[test]
    fn resync_retitles_existing_entries() {
        let mut store = Store::open_in_memory().unwrap();
        let mut snap = snapshot(vec![], vec![]);
        snap.entries[0].title_cache = Some("Old".into());
        store
            .import_snapshot(&snap, &ImportExtras::default())
            .unwrap();

        snap.entries[0].title_cache = Some("New".into());
        snap.entries[1].title_cache = None; // no title: leave B alone
        let plan = store.plan_resync(&snap, &ImportExtras::default()).unwrap();
        assert_eq!(plan.entries_to_retitle.len(), 1, "{plan:?}");
        assert_eq!(plan.entries_to_retitle[0].before.as_deref(), Some("Old"));
        store
            .apply_resync(&plan, &ImportExtras::default(), "test")
            .unwrap();
        let titles: Vec<_> = store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|e| (e.id.0, e.title_cache))
            .collect();
        assert!(
            titles.contains(&("A".into(), Some("New".into()))),
            "{titles:?}"
        );
        assert!(store
            .plan_resync(&snap, &ImportExtras::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn resync_rebinds_url_entries_and_swaps_cleanly() {
        let mut store = Store::open_in_memory().unwrap();
        let mut snap = snapshot(vec![], vec![]);
        snap.entries[0].store = "mal".into();
        snap.entries[0].store_id = "1".into();
        snap.entries[1].store = "url".into();
        snap.entries[1].store_id = "u:a".into();
        store
            .import_snapshot(&snap, &ImportExtras::default())
            .unwrap();

        // A takes B's URL while B moves on; a legacy id never rebinds.
        snap.entries[0].store = "url".into();
        snap.entries[0].store_id = "u:a".into();
        snap.entries[1].store_id = "u:b".into();
        let plan = store.plan_resync(&snap, &ImportExtras::default()).unwrap();
        assert_eq!(plan.entries_to_rebind.len(), 2, "{plan:?}");
        store
            .apply_resync(&plan, &ImportExtras::default(), "test")
            .unwrap();
        let bound: Vec<_> = store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|e| (e.id.0, e.store, e.store_id))
            .collect();
        assert!(bound.contains(&("A".into(), "url".into(), "u:a".into())), "{bound:?}");
        assert!(bound.contains(&("B".into(), "url".into(), "u:b".into())), "{bound:?}");
        assert!(store
            .plan_resync(&snap, &ImportExtras::default())
            .unwrap()
            .is_empty());

        snap.entries[1].store = "mal".into();
        snap.entries[1].store_id = "2".into();
        assert!(store
            .plan_resync(&snap, &ImportExtras::default())
            .unwrap()
            .entries_to_rebind
            .is_empty());
    }

    /// A meme declared on A, split A 0.8 / B 0.2: the snapshot's two
    /// per-contributor impacts and the extras that say they're one.
    fn shared_meme(factor_a: f32, factor_b: f32) -> (Snapshot, ImportExtras) {
        let base = impact("base", "A", "meme", 5.0).atoms;
        let share = |id: &str, target: &str, factor: f32| Impact {
            id: ImpactId(id.into()),
            target: EntryId(target.into()),
            atoms: crate::shared::scale_atoms(&base, factor),
        };
        let snap = snapshot(
            vec![share("i1", "A", factor_a), share("i2", "B", factor_b)],
            vec![],
        );
        let mut extras = ImportExtras::default();
        extras
            .impact_share
            .insert(ImpactId("i1".into()), ("i1".into(), factor_a));
        extras
            .impact_share
            .insert(ImpactId("i2".into()), ("i1".into(), factor_b));
        extras.shared_base.insert("i1".into(), base);
        (snap, extras)
    }

    fn scored(store: &Store) -> Vec<(String, String, f32)> {
        let (snap, _) = store.load_snapshot().unwrap();
        let mut out: Vec<_> = snap
            .impacts
            .iter()
            .map(|i| {
                (
                    i.target.0.clone(),
                    i.atoms[0].blend[0].0 .0.clone(),
                    i.atoms[0].magnitude,
                )
            })
            .collect();
        out.sort_by(|a, b| a.partial_cmp(b).unwrap());
        out
    }

    #[test]
    fn shared_impacts_import_once_and_score_per_member() {
        let (snap, extras) = shared_meme(0.8, 0.2);
        let mut store = Store::open_in_memory().unwrap();
        store.import_snapshot(&snap, &extras).unwrap();
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM impact", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(
            scored(&store),
            [
                ("A".into(), "meme".into(), 5.0 * 0.8),
                ("B".into(), "meme".into(), 5.0 * 0.2)
            ]
        );
        let on_b = store.impacts_for_entry(&EntryId("B".into())).unwrap();
        assert_eq!(on_b.len(), 1);
        assert_eq!(on_b[0].dah_factor, Some(0.2));
        assert_eq!(on_b[0].members.len(), 2);
        assert!(store.plan_resync(&snap, &extras).unwrap().is_empty());
    }

    #[test]
    fn resync_folds_per_contributor_rows_into_a_shared_impact() {
        let (snap, extras) = shared_meme(0.8, 0.2);
        let mut store = Store::open_in_memory().unwrap();
        // An older database: the same shares as two ordinary impacts.
        store
            .import_snapshot(&snap, &ImportExtras::default())
            .unwrap();
        let before = scored(&store);

        let plan = store.plan_resync(&snap, &extras).unwrap();
        assert!(
            plan.impacts_to_add.is_empty() && plan.impacts_to_remove.is_empty(),
            "{plan:?}"
        );
        assert_eq!(plan.shared_changes.len(), 1);
        assert_eq!(
            plan.shared_changes[0].base,
            super::SharedBase::Fold("i1".into())
        );
        store.apply_resync(&plan, &extras, "test").unwrap();

        assert_eq!(scored(&store), before);
        assert_eq!(
            store.impact_members(&ImpactId("i1".into())).unwrap().len(),
            2
        );
        assert!(store.plan_resync(&snap, &extras).unwrap().is_empty());
    }

    #[test]
    fn resync_moves_only_the_changed_share() {
        let (snap, extras) = shared_meme(0.8, 0.2);
        let mut store = Store::open_in_memory().unwrap();
        store.import_snapshot(&snap, &extras).unwrap();

        let (snap, extras) = shared_meme(0.8, 0.1);
        let plan = store.plan_resync(&snap, &extras).unwrap();
        assert_eq!(plan.impacts_to_remove.len(), 1, "{plan:?}");
        assert_eq!(plan.shared_changes.len(), 1);
        assert_eq!(
            plan.shared_changes[0].base,
            super::SharedBase::Existing("i1".into())
        );
        store.apply_resync(&plan, &extras, "test").unwrap();
        assert_eq!(
            scored(&store),
            [
                ("A".into(), "meme".into(), 5.0 * 0.8),
                ("B".into(), "meme".into(), 5.0 * 0.1)
            ]
        );
        assert!(store.plan_resync(&snap, &extras).unwrap().is_empty());
    }

    #[test]
    fn edited_shared_impacts_survive_resync() {
        let (snap, extras) = shared_meme(0.8, 0.2);
        let mut store = Store::open_in_memory().unwrap();
        store.import_snapshot(&snap, &extras).unwrap();
        let members = vec![
            crate::shared::ImpactMember {
                contributor: EntryId("A".into()),
                factor: 0.5,
            },
            crate::shared::ImpactMember {
                contributor: EntryId("B".into()),
                factor: 0.5,
            },
        ];
        let base = impact("base", "A", "meme", 5.0).atoms;
        store
            .edit_shared_impact(&ImpactId("i1".into()), &base, &members, None)
            .unwrap();

        let plan = store.plan_resync(&snap, &extras).unwrap();
        assert!(plan.impacts_to_remove.is_empty(), "{plan:?}");
        assert!(plan.impacts_to_add.is_empty(), "{plan:?}");
        assert!(plan.shared_changes.is_empty(), "{plan:?}");
        assert_eq!(plan.impacts_kept.len(), 2);
    }

    #[test]
    fn stored_splits_flatten_rebalance_and_survive_resync() {
        use crate::split::{SplitNode, SplitRule};
        let (snap, extras) = shared_meme(0.8, 0.2);
        let mut store = Store::open_in_memory().unwrap();
        store.import_snapshot(&snap, &extras).unwrap();
        let id = ImpactId("i1".into());
        // A 0.4 explicit, B alone in an even group worth 0.6.
        let tree = SplitNode::Group {
            rule: SplitRule::Explicit,
            label: None,
            share: 0.0,
            weight: 0.0,
            children: vec![
                SplitNode::Member { entry: EntryId("A".into()), share: 0.4, weight: 0.0 },
                SplitNode::Group {
                    rule: SplitRule::Even,
                    label: Some("rest".into()),
                    share: 0.6,
                    weight: 0.0,
                    children: vec![SplitNode::Member { entry: EntryId("B".into()), share: 0.0, weight: 0.0 }],
                },
            ],
        };
        store.set_impact_split(&id, &tree).unwrap();
        assert_eq!(
            scored(&store),
            [("A".into(), "meme".into(), 5.0 * 0.4), ("B".into(), "meme".into(), 5.0 * 0.6)]
        );
        let on_a = store.impacts_for_entry(&EntryId("A".into())).unwrap();
        assert!(on_a[0].split_stored);
        assert_eq!(on_a[0].split.as_ref(), Some(&tree));

        // Editing values keeps the split.
        let base = impact("base", "A", "meme", 10.0).atoms;
        store.edit_shared_values(&id, &base, Some("why")).unwrap();
        assert_eq!(store.shared_impact(&id).unwrap().unwrap().split, tree);
        assert_eq!(
            scored(&store),
            [("A".into(), "meme".into(), 10.0 * 0.4), ("B".into(), "meme".into(), 10.0 * 0.6)]
        );

        // A hand-edited split is kept by resync.
        let plan = store.plan_resync(&snap, &extras).unwrap();
        assert!(plan.impacts_to_add.is_empty() && plan.impacts_to_remove.is_empty(), "{plan:?}");
        assert!(plan.shared_changes.is_empty(), "{plan:?}");

        // Removing B empties its group, which disappears; A stays 0.4.
        store.remove_impact_member(&id, &EntryId("B".into())).unwrap();
        assert_eq!(scored(&store), [("A".into(), "meme".into(), 10.0 * 0.4)]);
    }

    #[test]
    fn making_an_impact_shared_keeps_scores_and_resync_quiet() {
        let snap = snapshot(vec![impact("i1", "A", "comedy", 2.0)], vec![]);
        let mut store = Store::open_in_memory().unwrap();
        store.import_snapshot(&snap, &ImportExtras::default()).unwrap();
        let before = scored(&store);
        store.make_impact_shared(&ImpactId("i1".into())).unwrap();
        assert_eq!(scored(&store), before);
        let members = store.impact_members(&ImpactId("i1".into())).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!((members[0].contributor.0.as_str(), members[0].factor), ("A", 1.0));
        assert!(store.make_impact_shared(&ImpactId("i1".into())).is_err());
        assert!(store.plan_resync(&snap, &ImportExtras::default()).unwrap().is_empty());
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
