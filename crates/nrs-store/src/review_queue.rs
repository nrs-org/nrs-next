//! Read side of the `/review` queue (Part 3 of the palette-migration
//! plan): combines the confirmed `impact.note` SQL column with the
//! staging `ReviewLog` sidecar (`source_comment`/`needs_review`) --
//! nothing here is a SQL join, since half the data isn't in SQL.

use std::collections::BTreeSet;

use rusqlite::params;

use nrs_core::{Atom, EntryId, ImpactId, SourceId};

use crate::error::Result;
use crate::Store;

/// One impact as the review UI needs to see it: the graded atoms, the
/// confirmed `note` if any, and the raw suggestion (`source_comment`,
/// `needs_review`) still waiting on a human to accept or edit it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewImpact {
    pub id: ImpactId,
    pub target: EntryId,
    pub atoms: Vec<Atom>,
    pub note: Option<String>,
    pub source_comment: Option<String>,
    pub needs_review: bool,
    /// The DAH_standards kind (e.g. "nei") this impact was built from,
    /// if it was created or edited through the kind-based form. `None`
    /// for bulk-imported or raw-edited impacts, which have no such
    /// provenance.
    pub dah_kind: Option<String>,
    /// That kind's own parameters as a JSON object of form fields
    /// (e.g. `{"factor":"0.5","sign":"1"}`), alongside `dah_kind`.
    pub dah_params: Option<String>,
    /// The contributor share (see `dah_factor`'s schema comment) the
    /// kind editor multiplies the formula's recomputed output by
    /// before saving -- `None` alongside `dah_kind: None`; `Some(1.0)`
    /// for an unscaled DAH-built impact.
    pub dah_factor: Option<f32>,
    /// For a shared impact: every contributor and its share (this entry's
    /// own share is also `dah_factor`, and `atoms` are already scaled to
    /// it). Empty for an ordinary impact.
    pub members: Vec<crate::shared::ImpactMember>,
    /// For a shared impact: its split tree, stored (edited in the app) or
    /// inferred from `members`.
    pub split: Option<crate::split::SplitNode>,
    pub split_stored: bool,
}

impl Store {
    /// Every impact on `entry`, in insertion order: its own impacts, and
    /// its share of each shared impact it is a member of.
    pub fn impacts_for_entry(&self, entry: &EntryId) -> Result<Vec<ReviewImpact>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.note, i.dah_kind, i.dah_params, i.dah_factor, NULL, i.rowid \
               FROM impact i \
              WHERE i.target = ?1 \
                AND NOT EXISTS (SELECT 1 FROM impact_member m WHERE m.impact = i.id) \
             UNION ALL \
             SELECT i.id, i.note, i.dah_kind, i.dah_params, i.dah_factor, m.factor, i.rowid \
               FROM impact_member m JOIN impact i ON i.id = m.impact \
              WHERE m.contributor = ?1 \
             ORDER BY 7",
        )?;
        let rows = stmt.query_map(params![entry.0], |row| {
            let id: String = row.get(0)?;
            let note: Option<String> = row.get(1)?;
            let dah_kind: Option<String> = row.get(2)?;
            let dah_params: Option<String> = row.get(3)?;
            let dah_factor: Option<f32> = row.get(4)?;
            let share: Option<f32> = row.get(5)?;
            Ok((id, note, dah_kind, dah_params, dah_factor, share))
        })?;

        let mut impacts = Vec::new();
        for row in rows {
            let (id, note, dah_kind, dah_params, dah_factor, share) = row?;
            let (atoms, dah_factor, members) = match share {
                Some(share) => (
                    crate::shared::scale_atoms(&self.load_atoms(&id)?, share),
                    Some(share),
                    crate::shared::members_of(&self.conn, &id)?,
                ),
                None => (self.load_atoms(&id)?, dah_factor, Vec::new()),
            };
            let stored = if members.is_empty() {
                None
            } else {
                crate::shared::stored_split(&self.conn, &id)?
            };
            let split_stored = stored.is_some();
            let split = (!members.is_empty())
                .then(|| stored.unwrap_or_else(|| crate::split::infer(&members)));
            let review = self.review.get(&id);
            impacts.push(ReviewImpact {
                id: ImpactId(id),
                target: entry.clone(),
                atoms,
                note,
                source_comment: review.source_comment,
                needs_review: review.needs_review,
                dah_kind,
                dah_params,
                dah_factor,
                members,
                split,
                split_stored,
            });
        }
        Ok(impacts)
    }

    /// Entries with something left to confirm: an anchor-ambiguous
    /// impact (`needs_review`), or just an unconfirmed comment (`note`
    /// still unset but `ReviewLog` has a `source_comment` suggestion) --
    /// the broadened scope from the plan, not only anchor ambiguity.
    /// Order follows the first flagged impact's insertion order, not
    /// entry id -- there's no meaningful independent ordering for entries
    /// here.
    pub fn entries_needing_review(&self) -> Result<Vec<EntryId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, target, note FROM impact ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let target: String = row.get(1)?;
            let note: Option<String> = row.get(2)?;
            Ok((id, target, note))
        })?;

        // A shared impact is reviewed from each member's page.
        let members = crate::shared::load_all_members(&self.conn)?;
        let mut seen = BTreeSet::new();
        let mut flagged = Vec::new();
        for row in rows {
            let (id, target, note) = row?;
            let review = self.review.get(&id);
            let has_unconfirmed_comment = note.is_none() && review.source_comment.is_some();
            if !(review.needs_review || has_unconfirmed_comment) {
                continue;
            }
            let targets: Vec<String> = match members.get(&id) {
                Some(members) => members.iter().map(|m| m.contributor.0.clone()).collect(),
                None => vec![target],
            };
            for target in targets {
                if seen.insert(target.clone()) {
                    flagged.push(EntryId(target));
                }
            }
        }
        Ok(flagged)
    }

    fn load_atoms(&self, impact_id: &str) -> Result<Vec<Atom>> {
        let mut stmt = self.conn.prepare(
            "SELECT source, blend, magnitude, time FROM impact_atom \
             WHERE impact = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![impact_id], |row| {
            let source: String = row.get(0)?;
            let blend: String = row.get(1)?;
            let magnitude: f32 = row.get(2)?;
            let time: String = row.get(3)?;
            Ok((source, blend, magnitude, time))
        })?;
        let mut atoms = Vec::new();
        for row in rows {
            let (source, blend, magnitude, time) = row?;
            atoms.push(Atom {
                source: SourceId(source),
                blend: serde_json::from_str(&blend)?,
                magnitude,
                time: serde_json::from_str(&time)?,
            });
        }
        Ok(atoms)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nrs_core::{AnchorId, Entry, Impact, Palette, Snapshot, TimeSupport};

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

    /// Two entries: `e1` has one anchor-ambiguous impact and one plain
    /// one; `e2` has a single unambiguous, uncommented impact.
    fn seeded_store() -> Store {
        let mut store = Store::open_in_memory().unwrap();
        let e1 = EntryId("e1".into());
        let e2 = EntryId("e2".into());
        let snap = Snapshot {
            entries: vec![
                Entry {
                    id: e1.clone(),
                    store: "music".into(),
                    store_id: "m1".into(),
                    title_cache: None,
                },
                Entry {
                    id: e2.clone(),
                    store: "music".into(),
                    store_id: "m2".into(),
                    title_cache: None,
                },
            ],
            impacts: vec![
                Impact {
                    id: ImpactId("i1".into()),
                    target: e1.clone(),
                    atoms: vec![sample_atom("comedy", "src-a", 1.0)],
                },
                Impact {
                    id: ImpactId("i2".into()),
                    target: e1,
                    atoms: vec![sample_atom("hate", "src-b", 1.0)],
                },
                Impact {
                    id: ImpactId("i3".into()),
                    target: e2,
                    atoms: vec![sample_atom("hooked", "src-c", 1.0)],
                },
            ],
            relations: vec![],
            palette: Palette {
                anchors: vec![],
                k_feat: Default::default(),
            },
        };
        let mut comments = BTreeMap::new();
        comments.insert(ImpactId("i1".into()), "the hype is unreal".to_string());
        let mut needs_review = std::collections::BTreeSet::new();
        needs_review.insert(ImpactId("i1".into()));
        store
            .import_snapshot(
                &snap,
                &ImportExtras {
                    comments,
                    needs_review,
                    ..Default::default()
                },
            )
            .unwrap();
        store
    }

    #[test]
    fn impacts_for_entry_returns_atoms_and_review_state_together() {
        let store = seeded_store();
        let impacts = store.impacts_for_entry(&EntryId("e1".into())).unwrap();

        assert_eq!(impacts.len(), 2);
        assert_eq!(impacts[0].id, ImpactId("i1".into()));
        assert_eq!(
            impacts[0].source_comment.as_deref(),
            Some("the hype is unreal")
        );
        assert!(impacts[0].needs_review);
        assert_eq!(impacts[0].note, None);

        assert_eq!(impacts[1].id, ImpactId("i2".into()));
        assert!(!impacts[1].needs_review);
        assert_eq!(impacts[1].source_comment, None);
    }

    #[test]
    fn entries_needing_review_flags_ambiguous_anchor_but_not_plain_entries() {
        let store = seeded_store();
        let flagged = store.entries_needing_review().unwrap();
        assert_eq!(flagged, vec![EntryId("e1".into())]);
    }

    #[test]
    fn entries_needing_review_also_flags_a_bare_unconfirmed_comment() {
        let mut store = Store::open_in_memory().unwrap();
        let entry = EntryId("e1".into());
        let snap = Snapshot {
            entries: vec![Entry {
                id: entry.clone(),
                store: "music".into(),
                store_id: "m1".into(),
                title_cache: None,
            }],
            impacts: vec![Impact {
                id: ImpactId("i1".into()),
                target: entry.clone(),
                atoms: vec![sample_atom("hooked", "src-a", 1.0)],
            }],
            relations: vec![],
            palette: Palette {
                anchors: vec![],
                k_feat: Default::default(),
            },
        };
        let mut comments = BTreeMap::new();
        comments.insert(ImpactId("i1".into()), "comfy af".to_string());
        store
            .import_snapshot(
                &snap,
                &ImportExtras {
                    comments,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(store.entries_needing_review().unwrap(), vec![entry.clone()]);

        store.confirm_impact(&ImpactId("i1".into())).unwrap();
        assert_eq!(
            store.entries_needing_review().unwrap(),
            Vec::<EntryId>::new()
        );
    }
}
