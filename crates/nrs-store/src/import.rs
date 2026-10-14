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

use nrs_core::{EntryId, ImpactId, RelKind, Relation, Snapshot};

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
    /// The raw `DAH_entry_roles` expression(s) behind each imported
    /// Credit relation, keyed by (contributor, work) -- see
    /// `nrs_import::snapshot::BuildExtras::credit_roles`. Stored into
    /// `relation.dah_params` as `{"roles": "..."}` so the Credit UI
    /// can show the roles rather than only the computed weight vector.
    pub credit_roles: BTreeMap<(EntryId, EntryId), String>,
    /// The DAH_standards kind + form params each imported impact's
    /// atoms were derived from, plus its `<contributor factor="..">`
    /// share -- see `nrs_import::snapshot::BuildExtras::impact_dah`.
    /// Stored into `impact.dah_kind`/`dah_params`/`dah_factor` so
    /// nrs-app's kind-based editor works on bulk-imported impacts too,
    /// not only ones created through it.
    pub impact_dah: BTreeMap<ImpactId, (String, BTreeMap<String, String>, f32)>,
    /// Snapshot impacts that are one contributor's share of a shared
    /// impact: impact id -> (shared key, share). All impacts with one key
    /// came from one XML element and are stored as one shared impact.
    pub impact_share: BTreeMap<ImpactId, (String, f32)>,
    /// Shared key -> the unscaled base atoms, exactly as the importer
    /// computed them before splitting, so expansion reproduces the
    /// snapshot's per-contributor values bit for bit.
    pub shared_base: BTreeMap<String, Vec<nrs_core::Atom>>,
}

/// Where a shared impact lives when it isn't scored directly: the entry
/// that declared it (its atoms' source) if that is an entry, else its
/// first member.
pub(crate) fn shared_home(
    base: &[nrs_core::Atom],
    first_member: &EntryId,
    known: &BTreeSet<String>,
) -> EntryId {
    base.first()
        .map(|atom| atom.source.0.clone())
        .filter(|source| known.contains(source))
        .map(EntryId)
        .unwrap_or_else(|| first_member.clone())
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

fn insert_relation(tx: &Transaction, relation: &Relation, dah_params: Option<&str>) -> Result<()> {
    // `Relation` carries no id (INFRA.md's frozen contract) — mint a
    // store-only one, same as `add_relation`.
    let id = Ulid::new().to_string();
    let transform = serde_json::to_string(&relation.transform)?;
    tx.execute(
        "INSERT INTO relation (id, kind, from_e, to_e, transform, dah_params) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            rel_kind_to_str(relation.kind),
            relation.from.0,
            relation.to.0,
            transform,
            dah_params
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

        // The XML parser sometimes emits the same `<entry id=...>` node
        // twice into `snap.entries` for one logical entry (it warns
        // "declared 2 times... merged into one entry" — the *content*
        // gets merged, but the entry row itself doesn't) — skip the
        // repeat rather than violating the `id` primary key.
        let mut seen_ids = BTreeSet::new();
        // Separately, legacy XML occasionally cites the same type-store
        // entry from two genuinely distinct entries (a copy-paste in
        // `nrs-impl`'s source data). `(store, store_id)` is UNIQUE, so a
        // second claimant falls back to the same "no known store entry"
        // shape unclaimed entries already use — `entry.id` is the raw XML
        // id and is guaranteed unique on its own.
        let mut seen_external = BTreeSet::new();
        for entry in &snap.entries {
            if !seen_ids.insert(entry.id.0.clone()) {
                continue;
            }
            let (store, store_id) =
                if seen_external.insert((entry.store.clone(), entry.store_id.clone())) {
                    (entry.store.as_str(), entry.store_id.as_str())
                } else {
                    ("none", entry.id.0.as_str())
                };
            tx.execute(
                "INSERT INTO entry (id, store, store_id, title_cache) VALUES (?1, ?2, ?3, ?4)",
                params![entry.id.0, store, store_id, entry.title_cache],
            )?;
        }

        let known: BTreeSet<String> = snap.entries.iter().map(|e| e.id.0.clone()).collect();
        let mut shared_written: BTreeSet<&str> = BTreeSet::new();
        let mut review_states = Vec::with_capacity(snap.impacts.len());
        for impact in &snap.impacts {
            // A contributor's share of a shared impact: write the shared
            // impact once (keyed by its first member's id), then the member.
            let share = extras.impact_share.get(&impact.id);
            if let Some((key, factor)) = share {
                if shared_written.insert(key.as_str()) {
                    let base = extras.shared_base.get(key).ok_or_else(|| {
                        crate::error::StoreError::InvalidData(format!(
                            "no base atoms for shared impact {key}"
                        ))
                    })?;
                    let (dah_kind, dah_params) = match extras.impact_dah.get(&impact.id) {
                        Some((kind, params, _)) => {
                            (Some(kind.as_str()), Some(serde_json::to_string(params)?))
                        }
                        None => (None, None),
                    };
                    tx.execute(
                        "INSERT INTO impact (id, target, note, created, dah_kind, dah_params, dah_factor) \
                         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
                        params![
                            key,
                            shared_home(base, &impact.target, &known).0,
                            now_ts(),
                            dah_kind,
                            dah_params,
                            dah_kind.map(|_| 1.0f32)
                        ],
                    )?;
                    for atom in base {
                        tx.execute(
                            "INSERT INTO impact_atom (impact, source, blend, magnitude, time) \
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                key,
                                atom.source.0,
                                serde_json::to_string(&atom.blend)?,
                                atom.magnitude,
                                serde_json::to_string(&atom.time)?
                            ],
                        )?;
                    }
                    let source_comment = extras.comments.get(&impact.id).cloned();
                    let needs_review = extras.needs_review.contains(&impact.id);
                    if source_comment.is_some() || needs_review {
                        review_states.push((
                            key.clone(),
                            ReviewState {
                                source_comment,
                                needs_review,
                            },
                        ));
                    }
                }
                tx.execute(
                    "INSERT INTO impact_member (impact, contributor, factor) VALUES (?1, ?2, ?3)",
                    params![key, impact.target.0, factor],
                )?;
                continue;
            }
            let dah = extras
                .impact_dah
                .get(&impact.id)
                .map(|(kind, params, factor)| {
                    Ok::<_, crate::error::StoreError>((
                        kind,
                        serde_json::to_string(params)?,
                        *factor,
                    ))
                })
                .transpose()?;
            let (dah_kind, dah_params, dah_factor) = match &dah {
                Some((kind, params, factor)) => {
                    (Some(kind.as_str()), Some(params.as_str()), Some(*factor))
                }
                None => (None, None, None),
            };
            tx.execute(
                "INSERT INTO impact (id, target, note, created, dah_kind, dah_params, dah_factor) \
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
                params![
                    impact.id.0,
                    impact.target.0,
                    now_ts(),
                    dah_kind,
                    dah_params,
                    dah_factor
                ],
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
            let dah_params = if relation.kind == RelKind::Credit {
                extras
                    .credit_roles
                    .get(&(relation.from.clone(), relation.to.clone()))
                    .map(|roles| serde_json::json!({ "roles": roles }).to_string())
            } else {
                None
            };
            insert_relation(&tx, relation, dah_params.as_deref())?;
        }

        for anchor in &snap.palette.anchors {
            tx.execute(
                "INSERT INTO anchor (id, label, family) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(id) DO UPDATE SET label = excluded.label, family = excluded.family",
                params![anchor.id.0, anchor.label, anchor.family],
            )?;
        }
        // The schema seeds the default `K`; an imported palette overrides
        // the entries it lists.
        for ((a, b), k) in &snap.palette.k_feat.pairs {
            tx.execute(
                "INSERT INTO kfeat (a, b, k) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(a, b) DO UPDATE SET k = excluded.k",
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
        Atom, Entry, EntryId, Impact, ImpactId, Palette, Snapshot, SourceId, TimeSupport,
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
            ..Default::default()
        };

        store.import_snapshot(&snap, &extras).unwrap();

        let flagged = store.review.get("i1");
        assert_eq!(
            flagged.source_comment.as_deref(),
            Some("the hype is unreal")
        );
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
