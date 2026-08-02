//! Bulk snapshot ingestion (INFRA.md roadmap phase 3: XML -> `nrs.db`).
//! Unlike the interactive CRUD in `crud.rs`, ids are preserved exactly as
//! given — the importer's `Entry`/`Impact` ids come from the legacy XML
//! source, not freshly-minted ULIDs, and `Impact.target` / `Relation.from`
//! / `Relation.to` reference them by that exact string — so this can't go
//! through `add_entry`/`add_impact`, which mint their own ids.

use rusqlite::{params, Transaction};
use serde::Serialize;
use ulid::Ulid;

use nrs_core::{Relation, Snapshot};

use crate::error::Result;
use crate::event::EventKind;
use crate::relkind::rel_kind_to_str;
use crate::Store;

fn now_ts() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[derive(Serialize)]
struct ImportSummary {
    entries: usize,
    impacts: usize,
    relations: usize,
    anchors: usize,
}

fn insert_relation(tx: &Transaction, relation: &Relation) -> Result<()> {
    // `Relation` carries no id (INFRA.md's frozen contract) — mint a
    // store-only one, same as `add_relation`.
    let id = Ulid::new().to_string();
    let transform = serde_json::to_string(&relation.transform)?;
    tx.execute(
        "INSERT INTO relation (id, kind, from_e, to_e, transform) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id,
            rel_kind_to_str(relation.kind),
            relation.from.0,
            relation.to.0,
            transform
        ],
    )?;
    Ok(())
}

impl Store {
    /// Writes an imported `Snapshot` into this store as one transaction,
    /// preserving every id exactly as given, and appends a single
    /// `snapshot.import` event summarizing what was written. Intended for
    /// a fresh/empty `nrs.db` — importing into an already-populated store
    /// will collide on entry ids. Leaves `params`/`calibration` untouched;
    /// set those separately if needed.
    pub fn import_snapshot(&mut self, snap: &Snapshot) -> Result<()> {
        let tx = self.conn.transaction()?;

        for entry in &snap.entries {
            tx.execute(
                "INSERT INTO entry (id, store, store_id, title_cache) VALUES (?1, ?2, ?3, ?4)",
                params![entry.id.0, entry.store, entry.store_id, entry.title_cache],
            )?;
        }

        for impact in &snap.impacts {
            tx.execute(
                "INSERT INTO impact (id, target, note, created) VALUES (?1, ?2, NULL, ?3)",
                params![impact.id.0, impact.target.0, now_ts()],
            )?;
            for atom in &impact.atoms {
                let blend = serde_json::to_string(&atom.blend)?;
                let time = serde_json::to_string(&atom.time)?;
                tx.execute(
                    "INSERT INTO impact_atom (impact, source, blend, magnitude, time) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![impact.id.0, atom.source.0, blend, atom.magnitude, time],
                )?;
            }
        }

        for relation in &snap.relations {
            insert_relation(&tx, relation)?;
        }

        for anchor in &snap.palette.anchors {
            tx.execute(
                "INSERT INTO anchor (id, label, family) VALUES (?1, ?2, ?3)",
                params![anchor.id.0, anchor.label, anchor.family],
            )?;
        }
        for ((a, b), k) in &snap.palette.k_feat.pairs {
            tx.execute(
                "INSERT INTO kfeat (a, b, k) VALUES (?1, ?2, ?3)",
                params![a.0, b.0, k],
            )?;
        }

        let after = serde_json::to_string(&ImportSummary {
            entries: snap.entries.len(),
            impacts: snap.impacts.len(),
            relations: snap.relations.len(),
            anchors: snap.palette.anchors.len(),
        })?;
        tx.execute(
            "INSERT INTO event (ts, kind, target, before, after, note) \
             VALUES (?1, ?2, NULL, NULL, ?3, NULL)",
            params![now_ts(), EventKind::SnapshotImport.as_str(), after],
        )?;

        tx.commit()?;
        Ok(())
    }
}
