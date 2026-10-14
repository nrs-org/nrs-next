//! Shared impacts (schema V8).
//!
//! An impact split between several entries (an XML impact with
//! `<contributor>` children, a meme spread over a franchise) is stored
//! once: the `impact` row holds the unscaled base atoms, and
//! `impact_member` holds each contributor's share. Readers that need one
//! impact per entry (scoring, the entry page) get an expanded copy per
//! member: base atoms x factor, computed exactly as the importer splits
//! (`magnitude *= factor` in `f32`), so expansion reproduces imported
//! values bit for bit. Shares need not sum to 1.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use nrs_core::{Atom, EntryId, Impact, ImpactId};

use crate::crud::{append_event, to_json};
use crate::split::{self, SplitNode};
use crate::error::{Result, StoreError};
use crate::event::EventKind;
use crate::Store;

/// One contributor's share of a shared impact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactMember {
    pub contributor: EntryId,
    pub factor: f32,
}

/// Separates a shared impact's id from a contributor in the id of its
/// expanded per-contributor copy (`i223~A-MAL-56653`).
pub const MEMBER_ID_SEP: char = '~';

pub fn member_impact_id(base: &str, contributor: &str) -> String {
    format!("{base}{MEMBER_ID_SEP}{contributor}")
}

/// `(shared impact id, contributor)` for an expanded copy's id.
pub fn split_member_impact_id(id: &str) -> Option<(&str, &str)> {
    id.split_once(MEMBER_ID_SEP)
}

/// `atoms` as one contributor's share.
pub fn scale_atoms(atoms: &[Atom], factor: f32) -> Vec<Atom> {
    atoms
        .iter()
        .cloned()
        .map(|mut atom| {
            atom.magnitude *= factor;
            atom
        })
        .collect()
}

/// The per-contributor copies of a shared impact, in member order.
pub fn expand(base: &Impact, members: &[ImpactMember]) -> Vec<Impact> {
    members
        .iter()
        .map(|member| Impact {
            id: ImpactId(member_impact_id(&base.id.0, &member.contributor.0)),
            target: member.contributor.clone(),
            atoms: scale_atoms(&base.atoms, member.factor),
        })
        .collect()
}

/// An impact as the event log records it: the `Impact` fields, plus the
/// member list when it is shared. Readers that only know `Impact` ignore
/// `members`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImpactRecord {
    #[serde(flatten)]
    pub impact: Impact,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<ImpactMember>,
    /// The stored split tree, when one was edited in the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitNode>,
}

impl ImpactRecord {
    /// What scoring saw: the impact itself, or its expanded copies.
    pub fn expanded(&self) -> Vec<Impact> {
        if self.members.is_empty() {
            vec![self.impact.clone()]
        } else {
            expand(&self.impact, &self.members)
        }
    }
}

/// Every shared impact's members, keyed by impact id, in insertion order.
pub(crate) fn load_all_members(conn: &Connection) -> Result<BTreeMap<String, Vec<ImpactMember>>> {
    let mut stmt =
        conn.prepare("SELECT impact, contributor, factor FROM impact_member ORDER BY rowid")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f32>(2)?,
        ))
    })?;
    let mut out: BTreeMap<String, Vec<ImpactMember>> = BTreeMap::new();
    for row in rows {
        let (impact, contributor, factor) = row?;
        out.entry(impact).or_default().push(ImpactMember {
            contributor: EntryId(contributor),
            factor,
        });
    }
    Ok(out)
}

pub(crate) fn members_of(conn: &Connection, impact: &str) -> Result<Vec<ImpactMember>> {
    let mut stmt = conn.prepare(
        "SELECT contributor, factor FROM impact_member WHERE impact = ?1 ORDER BY rowid",
    )?;
    let rows = stmt.query_map(params![impact], |row| {
        Ok(ImpactMember {
            contributor: EntryId(row.get(0)?),
            factor: row.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

pub(crate) fn insert_members(
    tx: &Transaction,
    impact: &str,
    members: &[ImpactMember],
) -> Result<()> {
    for member in members {
        tx.execute(
            "INSERT INTO impact_member (impact, contributor, factor) VALUES (?1, ?2, ?3)",
            params![impact, member.contributor.0, member.factor],
        )?;
    }
    Ok(())
}

pub(crate) fn validate_members(members: &[ImpactMember]) -> Result<()> {
    if members.is_empty() {
        return Err(StoreError::Invalid(
            "a shared impact needs at least one member".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for member in members {
        if !member.factor.is_finite() || member.factor < 0.0 {
            return Err(StoreError::Invalid(format!(
                "member {}: share must be a finite number >= 0",
                member.contributor.0
            )));
        }
        if !seen.insert(&member.contributor) {
            return Err(StoreError::Invalid(format!(
                "member {} listed twice",
                member.contributor.0
            )));
        }
    }
    Ok(())
}

/// The event-log form of impact `id` (with members if shared).
pub(crate) fn read_record(tx: &Connection, id: &ImpactId) -> Result<Option<ImpactRecord>> {
    let target: Option<String> = tx
        .query_row(
            "SELECT target FROM impact WHERE id = ?1",
            params![id.0],
            |row| row.get(0),
        )
        .optional()?;
    let Some(target) = target else {
        return Ok(None);
    };
    let mut stmt = tx.prepare(
        "SELECT source, blend, magnitude, time FROM impact_atom WHERE impact = ?1 ORDER BY rowid",
    )?;
    let rows = stmt.query_map(params![id.0], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f32>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut atoms = Vec::new();
    for row in rows {
        let (source, blend, magnitude, time) = row?;
        atoms.push(Atom {
            source: nrs_core::SourceId(source),
            blend: serde_json::from_str(&blend)?,
            magnitude,
            time: serde_json::from_str(&time)?,
        });
    }
    Ok(Some(ImpactRecord {
        impact: Impact {
            id: id.clone(),
            target: EntryId(target),
            atoms,
        },
        members: members_of(tx, &id.0)?,
        split: stored_split(tx, &id.0)?,
    }))
}

/// The split tree stored for `impact`, if it was edited in the app.
pub(crate) fn stored_split(conn: &Connection, impact: &str) -> Result<Option<SplitNode>> {
    let tree: Option<String> = conn
        .query_row("SELECT tree FROM impact_split WHERE impact = ?1", params![impact], |row| row.get(0))
        .optional()?;
    tree.map(|t| serde_json::from_str(&t)).transpose().map_err(Into::into)
}

/// Stores `tree` for `impact` and replaces its members with the tree's
/// flattened shares.
pub(crate) fn write_split(tx: &Transaction, impact: &str, tree: &SplitNode) -> Result<()> {
    tx.execute(
        "INSERT INTO impact_split (impact, tree) VALUES (?1, ?2) \
         ON CONFLICT(impact) DO UPDATE SET tree = excluded.tree",
        params![impact, serde_json::to_string(tree)?],
    )?;
    tx.execute("DELETE FROM impact_member WHERE impact = ?1", params![impact])?;
    insert_members(tx, impact, &split::flatten(tree))
}

/// A shared impact as stored: unscaled base atoms, note, members, and its
/// split tree (stored, or inferred from the members).
#[derive(Debug, Clone, PartialEq)]
pub struct SharedImpact {
    pub atoms: Vec<Atom>,
    pub note: Option<String>,
    pub members: Vec<ImpactMember>,
    pub split: SplitNode,
    pub split_stored: bool,
}

impl Store {
    /// Shared impact `id` with its unscaled base, or `None` if `id` is not
    /// a shared impact.
    pub fn shared_impact(&self, id: &ImpactId) -> Result<Option<SharedImpact>> {
        let Some(record) = read_record(&self.conn, id)? else {
            return Ok(None);
        };
        if record.members.is_empty() {
            return Ok(None);
        }
        let note: Option<String> = self.conn.query_row(
            "SELECT note FROM impact WHERE id = ?1",
            params![id.0],
            |row| row.get(0),
        )?;
        let split_stored = record.split.is_some();
        let split = record.split.unwrap_or_else(|| split::infer(&record.members));
        Ok(Some(SharedImpact {
            atoms: record.impact.atoms,
            note,
            members: record.members,
            split,
            split_stored,
        }))
    }

    /// The members of shared impact `id` (empty for an ordinary impact).
    pub fn impact_members(&self, id: &ImpactId) -> Result<Vec<ImpactMember>> {
        members_of(&self.conn, &id.0)
    }

    /// Replaces a shared impact's base atoms and member shares in one
    /// write and one `impact.edit` event. `atoms` are unscaled; each
    /// member gets `atoms x factor`.
    pub fn edit_shared_impact(
        &mut self,
        id: &ImpactId,
        atoms: &[Atom],
        members: &[ImpactMember],
        note: Option<&str>,
    ) -> Result<()> {
        validate_members(members)?;
        let tx = self.conn.transaction()?;
        let before = read_record(&tx, id)?
            .ok_or_else(|| StoreError::Invalid(format!("no impact {}", id.0)))?;
        if before.members.is_empty() {
            return Err(StoreError::Invalid(format!(
                "impact {} is not shared",
                id.0
            )));
        }
        tx.execute("DELETE FROM impact_atom WHERE impact = ?1", params![id.0])?;
        for atom in atoms {
            tx.execute(
                "INSERT INTO impact_atom (impact, source, blend, magnitude, time) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id.0,
                    atom.source.0,
                    serde_json::to_string(&atom.blend)?,
                    atom.magnitude,
                    serde_json::to_string(&atom.time)?
                ],
            )?;
        }
        tx.execute("DELETE FROM impact_member WHERE impact = ?1", params![id.0])?;
        insert_members(&tx, &id.0, members)?;
        tx.execute(
            "UPDATE impact SET note = ?2 WHERE id = ?1",
            params![id.0, note],
        )?;
        let after = read_record(&tx, id)?;
        append_event(
            &tx,
            EventKind::ImpactEdit,
            Some(&id.0),
            to_json(Some(&before))?.as_deref(),
            to_json(after.as_ref())?.as_deref(),
            note,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces shared impact `id`'s split with `tree` (stored, and
    /// flattened into its member shares) in one write and one event.
    pub fn set_impact_split(&mut self, id: &ImpactId, tree: &SplitNode) -> Result<()> {
        split::validate(tree).map_err(StoreError::Invalid)?;
        validate_members(&split::flatten(tree))?;
        let tx = self.conn.transaction()?;
        let before = read_record(&tx, id)?
            .ok_or_else(|| StoreError::Invalid(format!("no impact {}", id.0)))?;
        if before.members.is_empty() {
            return Err(StoreError::Invalid(format!("impact {} is not shared", id.0)));
        }
        write_split(&tx, &id.0, tree)?;
        let after = read_record(&tx, id)?;
        append_event(
            &tx,
            EventKind::ImpactEdit,
            Some(&id.0),
            to_json(Some(&before))?.as_deref(),
            to_json(after.as_ref())?.as_deref(),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Turns ordinary impact `id` into a shared impact whose only member
    /// is its current target, at its current share (`dah_factor`, 1 if
    /// unset): the stored atoms are unscaled into the base, so scoring is
    /// unchanged until members are added.
    pub fn make_impact_shared(&mut self, id: &ImpactId) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before = read_record(&tx, id)?
            .ok_or_else(|| StoreError::Invalid(format!("no impact {}", id.0)))?;
        if !before.members.is_empty() {
            return Err(StoreError::Invalid(format!("impact {} is already shared", id.0)));
        }
        let factor: Option<f32> = tx.query_row(
            "SELECT dah_factor FROM impact WHERE id = ?1",
            params![id.0],
            |row| row.get(0),
        )?;
        let factor = factor.filter(|f| *f > 0.0).unwrap_or(1.0);
        tx.execute(
            "UPDATE impact_atom SET magnitude = magnitude / ?2 WHERE impact = ?1",
            params![id.0, f64::from(factor)],
        )?;
        tx.execute(
            "UPDATE impact SET dah_factor = CASE WHEN dah_kind IS NULL THEN NULL ELSE 1.0 END WHERE id = ?1",
            params![id.0],
        )?;
        insert_members(
            &tx,
            &id.0,
            &[ImpactMember {
                contributor: before.impact.target.clone(),
                factor,
            }],
        )?;
        let after = read_record(&tx, id)?;
        append_event(
            &tx,
            EventKind::ImpactEdit,
            Some(&id.0),
            to_json(Some(&before))?.as_deref(),
            to_json(after.as_ref())?.as_deref(),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces a shared impact's base atoms and note, keeping its
    /// members and split as they are.
    pub fn edit_shared_values(&mut self, id: &ImpactId, atoms: &[Atom], note: Option<&str>) -> Result<()> {
        let members = members_of(&self.conn, &id.0)?;
        self.edit_shared_impact(id, atoms, &members, note)
    }

    /// Takes `contributor`'s share out of shared impact `id`; the impact
    /// goes away with its last member. A stored split rebalances by its
    /// groups' rules.
    pub fn remove_impact_member(&mut self, id: &ImpactId, contributor: &EntryId) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before = read_record(&tx, id)?
            .ok_or_else(|| StoreError::Invalid(format!("no impact {}", id.0)))?;
        if !before.members.iter().any(|m| &m.contributor == contributor) {
            return Err(StoreError::Invalid(format!(
                "{} is not a member of impact {}",
                contributor.0, id.0
            )));
        }
        let rebalanced = before.split.clone().and_then(|mut tree| {
            (split::remove_member(&mut tree, contributor) && split::validate(&tree).is_ok())
                .then_some(tree)
        });
        match rebalanced {
            Some(tree) => write_split(&tx, &id.0, &tree)?,
            None => {
                tx.execute("DELETE FROM impact_split WHERE impact = ?1", params![id.0])?;
                tx.execute(
                    "DELETE FROM impact_member WHERE impact = ?1 AND contributor = ?2",
                    params![id.0, contributor.0],
                )?;
            }
        }
        let after = if members_of(&tx, &id.0)?.is_empty() {
            tx.execute("DELETE FROM impact_split WHERE impact = ?1", params![id.0])?;
            tx.execute("DELETE FROM impact_atom WHERE impact = ?1", params![id.0])?;
            tx.execute("DELETE FROM impact WHERE id = ?1", params![id.0])?;
            None
        } else {
            read_record(&tx, id)?
        };
        append_event(
            &tx,
            if after.is_some() {
                EventKind::ImpactEdit
            } else {
                EventKind::ImpactDelete
            },
            Some(&id.0),
            to_json(Some(&before))?.as_deref(),
            to_json(after.as_ref())?.as_deref(),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }
}
