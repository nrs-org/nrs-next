//! Bulk snapshot ingestion (INFRA.md roadmap phase 3: XML -> `nrs.db`).
//! Unlike the interactive CRUD in `crud.rs`, ids are preserved exactly as
//! given — the importer's `Entry`/`Impact` ids come from the legacy XML
//! source, not freshly-minted ULIDs, and `Impact.target` / `Relation.from`
//! / `Relation.to` reference them by that exact string — so this can't go
//! through `add_entry`/`add_impact`, which mint their own ids.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Transaction};
use serde::Serialize;
use ulid::Ulid;

use nrs_core::{ImpactId, Relation, Snapshot};

use crate::error::Result;
use crate::event::EventKind;
use crate::relkind::rel_kind_to_str;
use crate::review::ReviewState;
use crate::Store;

/// Staging data alongside a `Snapshot` that has nowhere to live in
/// `nrs-core` (not graded content) or `nrs.db` (not confirmed yet) --
/// see `review` module docs. Produced by `nrs_import::build_snapshot`.
#[derive(Debug, Default)]
pub struct ImportExtras {
    pub comments: BTreeMap<ImpactId, String>,
    pub needs_review: BTreeSet<ImpactId>,
}

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
    ///
    /// `extras` seeds the `ReviewLog` sidecar (raw comments + anchor-
    /// ambiguity flags) once the SQL transaction below commits — that
    /// seeding is a separate, non-atomic persistence step (see `review`
    /// module docs), so a review-log write failure after a successful
    /// commit leaves the `.db` import intact.
    pub fn import_snapshot(&mut self, snap: &Snapshot, extras: &ImportExtras) -> Result<()> {
        let tx = self.conn.transaction()?;

        for entry in &snap.entries {
            tx.execute(
                "INSERT INTO entry (id, store, store_id, title_cache) VALUES (?1, ?2, ?3, ?4)",
                params![entry.id.0, entry.store, entry.store_id, entry.title_cache],
            )?;
        }

        let mut review_states = Vec::with_capacity(snap.impacts.len());
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

            let source_comment = extras.comments.get(&impact.id).cloned();
            let needs_review = extras.needs_review.contains(&impact.id);
            if source_comment.is_some() || needs_review {
                review_states.push((
                    impact.id.0.clone(),
                    ReviewState {
                        source_comment,
                        needs_review,
                    },
                ));
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
        self.review.seed_bulk(review_states)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use nrs_core::{
        Atom, Entry, EntryId, Impact, ImpactId, Palette, Snapshot, SourceId, Taxonomy, TimeSupport,
    };

    use super::ImportExtras;
    use crate::Store;

    fn sample_snapshot() -> Snapshot {
        let entry = EntryId("e1".into());
        Snapshot {
            entries: vec![Entry {
                id: entry.clone(),
                store: "music".into(),
                store_id: "m1".into(),
                title_cache: None,
            }],
            impacts: vec![
                Impact {
                    id: ImpactId("i1".into()),
                    target: entry.clone(),
                    atoms: vec![Atom {
                        source: SourceId("s1".into()),
                        blend: vec![],
                        magnitude: 1.0,
                        time: TimeSupport::Moment,
                    }],
                },
                Impact {
                    id: ImpactId("i2".into()),
                    target: entry,
                    atoms: vec![],
                },
            ],
            relations: vec![],
            palette: Palette {
                anchors: vec![],
                k_feat: Default::default(),
            },
            taxonomy: Taxonomy::default(),
        }
    }

    #[test]
    fn import_seeds_review_log_only_for_impacts_with_comments_or_flags() {
        let mut store = Store::open_in_memory().unwrap();
        let snap = sample_snapshot();

        let mut comments = BTreeMap::new();
        comments.insert(ImpactId("i1".into()), "the hype is unreal".to_string());
        let mut needs_review = BTreeSet::new();
        needs_review.insert(ImpactId("i1".into()));
        let extras = ImportExtras {
            comments,
            needs_review,
        };

        store.import_snapshot(&snap, &extras).unwrap();

        let flagged = store.review.get("i1");
        assert_eq!(flagged.source_comment.as_deref(), Some("the hype is unreal"));
        assert!(flagged.needs_review);

        // i2 got neither a comment nor a needs-review flag -- untouched.
        let untouched = store.review.get("i2");
        assert_eq!(untouched.source_comment, None);
        assert!(!untouched.needs_review);
    }

    #[test]
    fn import_with_empty_extras_leaves_review_log_empty() {
        let mut store = Store::open_in_memory().unwrap();
        let snap = sample_snapshot();

        store
            .import_snapshot(&snap, &ImportExtras::default())
            .unwrap();

        assert_eq!(store.review.get("i1"), Default::default());
        assert_eq!(store.review.get("i2"), Default::default());
    }
}
