//! `nrs.db` -> `Snapshot` + `Params` (INFRA.md §5.1, §3 "load Snapshot").

use std::collections::BTreeMap;

use rusqlite::OptionalExtension;

use nrs_core::{
    Anchor, AnchorId, Atom, Entry, EntryId, Impact, ImpactId, Isotonic, KFeatMatrix, Palette,
    Params, Relation, Snapshot, SourceId, TimeSupport,
};

use crate::error::Result;
use crate::relkind::rel_kind_from_str;
use crate::Store;
use crate::{StoredParams, DEFAULT_MEDIUM};

impl Store {
    /// Loads the whole scoring substrate.
    pub fn load_snapshot(&self) -> Result<(Snapshot, Params)> {
        let entries = self.load_entries()?;
        let impacts = self.load_impacts()?;
        let relations = self.load_relations()?;
        let palette = self.load_palette()?;
        let params = self.load_params(&palette.k_feat)?;

        Ok((
            Snapshot {
                entries,
                impacts,
                relations,
                palette,
            },
            params,
        ))
    }

    /// Live entry list, bypassing whatever `Snapshot` an app-level
    /// caller loaded at startup — for UI that needs to see entries
    /// created since then (e.g. picking an import's target for a new
    /// impact/relation).
    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        self.load_entries()
    }

    fn load_entries(&self) -> Result<Vec<Entry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, store, store_id, title_cache FROM entry ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(Entry {
                id: EntryId(row.get(0)?),
                store: row.get(1)?,
                store_id: row.get(2)?,
                title_cache: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn load_impacts(&self) -> Result<Vec<Impact>> {
        let mut atoms_by_impact: BTreeMap<String, Vec<Atom>> = BTreeMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT impact, source, blend, magnitude, time \
                 FROM impact_atom ORDER BY impact, rowid",
            )?;
            let rows = stmt.query_map([], |row| {
                let impact: String = row.get(0)?;
                let source: String = row.get(1)?;
                let blend: String = row.get(2)?;
                let magnitude: f32 = row.get(3)?;
                let time: String = row.get(4)?;
                Ok((impact, source, blend, magnitude, time))
            })?;
            for row in rows {
                let (impact, source, blend, magnitude, time) = row?;
                let blend: Vec<(AnchorId, f32)> = serde_json::from_str(&blend)?;
                let time: TimeSupport = serde_json::from_str(&time)?;
                atoms_by_impact.entry(impact).or_default().push(Atom {
                    source: SourceId(source),
                    blend,
                    magnitude,
                    time,
                });
            }
        }

        let mut stmt = self
            .conn
            .prepare("SELECT id, target FROM impact ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let target: String = row.get(1)?;
            Ok((id, target))
        })?;
        // A shared impact scores as one copy per member (base x share).
        let mut members = crate::shared::load_all_members(&self.conn)?;
        let mut impacts = Vec::new();
        for row in rows {
            let (id, target) = row?;
            let atoms = atoms_by_impact.remove(&id).unwrap_or_default();
            let impact = Impact {
                id: ImpactId(id),
                target: EntryId(target),
                atoms,
            };
            match members.remove(&impact.id.0) {
                Some(members) => impacts.extend(crate::shared::expand(&impact, &members)),
                None => impacts.push(impact),
            }
        }
        Ok(impacts)
    }

    fn load_relations(&self) -> Result<Vec<Relation>> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, from_e, to_e, transform FROM relation ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let kind: String = row.get(0)?;
            let from: String = row.get(1)?;
            let to: String = row.get(2)?;
            let transform: String = row.get(3)?;
            Ok((kind, from, to, transform))
        })?;
        let mut relations = Vec::new();
        for row in rows {
            let (kind, from, to, transform) = row?;
            relations.push(Relation {
                kind: rel_kind_from_str(&kind)?,
                from: EntryId(from),
                to: EntryId(to),
                transform: serde_json::from_str(&transform)?,
            });
        }
        Ok(relations)
    }

    fn load_palette(&self) -> Result<Palette> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, label, family FROM anchor ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(Anchor {
                id: AnchorId(row.get(0)?),
                label: row.get(1)?,
                family: row.get(2)?,
            })
        })?;
        let anchors = rows.collect::<std::result::Result<Vec<_>, _>>()?;

        let mut stmt = self.conn.prepare("SELECT a, b, k FROM kfeat")?;
        let rows = stmt.query_map([], |row| {
            let a: String = row.get(0)?;
            let b: String = row.get(1)?;
            let k: f32 = row.get(2)?;
            Ok((a, b, k))
        })?;
        let mut pairs = BTreeMap::new();
        for row in rows {
            let (a, b, k) = row?;
            pairs.insert((AnchorId(a), AnchorId(b)), k);
        }
        Ok(Palette {
            anchors,
            k_feat: KFeatMatrix { pairs },
        })
    }

    fn load_params(&self, k_feat: &KFeatMatrix) -> Result<Params> {
        let isotonic: Option<String> = self
            .conn
            .query_row(
                "SELECT isotonic FROM calibration WHERE medium = ?1",
                [DEFAULT_MEDIUM],
                |row| row.get(0),
            )
            .optional()?;
        let calibration = match isotonic {
            Some(json) => serde_json::from_str(&json)?,
            None => Isotonic::default(),
        };

        let stored: Option<String> = self
            .conn
            .query_row("SELECT json FROM params WHERE id = 1", [], |row| row.get(0))
            .optional()?;
        let stored: StoredParams = match stored {
            Some(json) => serde_json::from_str(&json)?,
            None => StoredParams::default(),
        };

        Ok(Params {
            k_feat: k_feat.clone(),
            time: stored.time,
            calibration,
            sign: stored.sign,
        })
    }
}
