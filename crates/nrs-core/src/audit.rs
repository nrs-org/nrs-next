//! Monotonicity audit for `DegreeQ` (`redesign-tex/
//! monotonicity-vs-splitting-invariance.md` §1.4). DegreeQ
//! (`V = sum_i m_i c_i^{q-1}`) is splitting-invariant and satisfies
//! Property F, but is *not* unconditionally monotone: adding a positive
//! atom at a point `y` raises the crowding of every existing atom
//! related to `y`, and since `phi(c)=c^{q-1}` is decreasing for `q<1`,
//! each of those atoms loses a sliver of its own contribution. When the
//! aggregate loss exceeds the new atom's own gain, adding mass *lowers*
//! the score.
//!
//! The exact first-order (`eps -> 0+`) safety condition at insertion
//! point `y`, for a positive insertion, is
//!
//! ```text
//!   dV/deps|_0  =  C_y^{q-1}  +  (q-1) sum_i m_i kappa_i c_i^{q-2}  >= 0,
//!     C_y   = sum_i kappa_i m_i          (crowding at y from existing mass)
//!     kappa_i = K(x_i, y)                (self-inclusive: kappa=1 at y's own cell)
//!     c_i   = sum_j K(x_i,x_j) m_j       (existing self-inclusive crowding)
//! ```
//!
//! We split it into a nonnegative `gain = C_y^{q-1}` (the new atom's own
//! marginal credit) and `loss = (1-q) sum_i m_i kappa_i c_i^{q-2}` (the
//! marginal erosion of existing atoms). The insertion is locally
//! monotone iff `gain >= loss`; `ratio = gain/loss` is a scale-free
//! "how close to the failure zone" number (`>= 1` safe, `< 1` a real
//! local harm zone, `>> 1` comfortably safe).
//!
//! Only positive insertions are audited: a positive atom only crowds
//! same-signed (positive) existing atoms (`redundancy.rs`'s sign gate),
//! so it can only erode those. For `q >= 1` the `(q-1)` factor is
//! nonnegative and no erosion is possible, so the audit is a no-op there
//! and callers should only run it for `q < 1`.

use crate::measure::{build_measures, Measure};
use crate::redundancy::SourceIndex;
use crate::solve::solve_relations;
use crate::types::{
    AnchorId, EntryId, KFeatMatrix, KSrcLevels, Params, Relation, Snapshot, SourceId, Taxonomy,
};

/// One candidate insertion `(source, anchor)` into one entry, with its
/// first-order monotonicity margin.
#[derive(Debug, Clone)]
pub struct InsertionMargin {
    pub entry: EntryId,
    pub source: SourceId,
    pub anchor: AnchorId,
    /// `C_y^{q-1}` — the new atom's own marginal credit. Computed in
    /// f64: PowQ at low `q` drives crowding down to ~`1e-28`, where the
    /// `c^{q-2}` erosion terms overflow f32 (the same regime
    /// `redundancy.rs` floors to 0), so the true margin needs f64.
    pub gain: f64,
    /// `(1-q) sum_i m_i kappa_i c_i^{q-2}` — marginal erosion of the
    /// existing atoms this insertion is related to.
    pub loss: f64,
    /// `gain - loss = dV/deps|_0`. Negative ⇒ a real local harm zone:
    /// adding mass here strictly lowers the entry's score.
    pub marginal: f64,
    /// `gain / loss`. Scale-free closeness to the failure zone: `>= 1`
    /// safe, `< 1` a violation. `+inf` when nothing is eroded.
    pub ratio: f64,
    /// Crowding the insertion point already sees from existing mass.
    pub c_y: f64,
    /// How many *other* positive atoms this insertion is related to
    /// (`kappa > 0`) — the breadth of the erosion.
    pub n_related: usize,
    /// Smallest mass among the related existing atoms — the §1.4
    /// fragility driver (`m_i^{q-1} -> inf` as `m_i -> 0`).
    pub min_related_mass: f32,
}

/// Result of auditing one `(q, scaling)` setting across a snapshot.
#[derive(Debug, Clone)]
pub struct AuditReport {
    pub q: f32,
    pub n_entries_scored: usize,
    pub n_candidates: usize,
    /// Candidates that actually erode at least one *distinct* existing
    /// atom (`n_related > 0`). If this is 0, the cross-cell redundancy
    /// mechanism never engaged — every cell was isolated (no kernel
    /// relatedness between distinct cells), so the audit only exercised
    /// self-insertions and "0 violations" is trivial, not reassuring.
    pub n_bridging: usize,
    /// Candidates with `marginal < 0` (genuine local non-monotonicity).
    pub n_violations: usize,
    /// The single closest-to-(or past-)the-edge margin, by `ratio`.
    pub worst_ratio: f64,
    /// Worst candidates, ascending by `ratio` (most dangerous first).
    pub worst: Vec<InsertionMargin>,
}

fn kernel(
    s_i: &SourceId,
    a_i: &AnchorId,
    s_j: &SourceId,
    a_j: &AnchorId,
    k_feat: &KFeatMatrix,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    levels: &KSrcLevels,
) -> f32 {
    k_feat.get(a_i, a_j) * idx.k_src(s_i, s_j, taxonomy, levels)
}

/// Ancestors of `s` (walking `taxonomy.parent`), *excluding* `s`
/// itself, with a cycle/length guard.
fn ancestors(s: &SourceId, taxonomy: &Taxonomy) -> Vec<SourceId> {
    let mut out = Vec::new();
    let mut cur = s.clone();
    for _ in 0..64 {
        match taxonomy.parent.get(&cur) {
            Some(p) if !out.contains(p) && p != s => {
                out.push(p.clone());
                cur = p.clone();
            }
            _ => break,
        }
    }
    out
}

/// Audit every entry's resolved measure for first-order monotonicity
/// violations under `DegreeQ` at the given `params` (only `q`,
/// `k_feat`, `k_src_levels`, `relation_scaling` matter; `homogeneity`
/// and `calibration` are ignored).
///
/// Candidate insertion points, per entry, are the grid `S x A` where
/// `A` is the entry's own anchors and `S` is the entry's own sources
/// **plus all their taxonomy ancestors**. The ancestors are essential:
/// the §1.2 harm zone needs a point that *bridges* several existing
/// atoms which are otherwise less related to each other than to the
/// bridge — i.e. a common ancestor (a work-/franchise-level source) sat
/// at an anchor where many finer-grained sources reacted. Inserting only
/// at existing cells (the "log more of the same" edit) can never
/// reproduce it: a co-located insertion's own gain scales exactly like
/// the erosion it causes, pinning the ratio at `1/(1-q) >= 1`. `keep`
/// bounds how many worst margins are retained.
pub fn audit_monotonicity(snap: &Snapshot, params: &Params, keep: usize) -> AuditReport {
    let built = build_measures(snap);
    let resolved: std::collections::BTreeMap<EntryId, Measure> = solve_relations(
        built,
        snap.relations.as_slice() as &[Relation],
        params.q,
        params.relation_scaling,
    );

    let q = params.q;
    let k_feat = &params.k_feat;
    let levels = &params.k_src_levels;
    let tax = &snap.taxonomy;

    let mut all: Vec<InsertionMargin> = Vec::new();
    let mut n_candidates = 0usize;
    let mut n_bridging = 0usize;
    let mut n_violations = 0usize;
    let mut n_entries_scored = 0usize;
    let mut src_idx = SourceIndex::new();

    for (entry, f) in &resolved {
        // Positive cells only (a positive insertion crowds only
        // same-signed mass, per redundancy.rs's sign gate).
        let cells: Vec<(&SourceId, &AnchorId, f32)> = f
            .iter()
            .filter(|(_, m)| **m > 0.0)
            .map(|((s, a), m)| (s, a, *m))
            .collect();
        if cells.is_empty() {
            continue;
        }
        n_entries_scored += 1;

        // Self-inclusive crowding c_i for each positive cell (f64).
        let mut crowding: Vec<f64> = Vec::with_capacity(cells.len());
        for &(s_i, a_i, _) in &cells {
            let mut c = 0.0f64;
            for &(s_j, a_j, m_j) in &cells {
                c += f64::from(kernel(
                    s_i,
                    a_i,
                    s_j,
                    a_j,
                    k_feat,
                    tax,
                    &mut src_idx,
                    levels,
                )) * f64::from(m_j);
            }
            crowding.push(c);
        }

        // Candidate insertion grid: (entry's sources ∪ their ancestors)
        // × entry's anchors. Ancestors supply the bridge points §1.2
        // needs; de-duplicated across cells.
        let mut cand_sources: std::collections::BTreeSet<SourceId> =
            std::collections::BTreeSet::new();
        let mut cand_anchors: std::collections::BTreeSet<AnchorId> =
            std::collections::BTreeSet::new();
        for &(s, a, _) in &cells {
            cand_sources.insert(s.clone());
            for anc in ancestors(s, tax) {
                cand_sources.insert(anc);
            }
            cand_anchors.insert(a.clone());
        }

        for s_y in &cand_sources {
            for a_y in &cand_anchors {
                n_candidates += 1;
                let mut c_y = 0.0f64;
                let mut loss = 0.0f64;
                let mut n_related = 0usize;
                let mut min_related_mass = f32::INFINITY;
                let qd = f64::from(q);
                for (idx, &(s_i, a_i, m_i)) in cells.iter().enumerate() {
                    let kappa = f64::from(kernel(
                        s_i,
                        a_i,
                        s_y,
                        a_y,
                        k_feat,
                        tax,
                        &mut src_idx,
                        levels,
                    ));
                    if kappa == 0.0 {
                        continue;
                    }
                    let m_i64 = f64::from(m_i);
                    c_y += kappa * m_i64;
                    loss += m_i64 * kappa * crowding[idx].powf(qd - 2.0);
                    // "related" = a *distinct* atom being eroded (exclude
                    // the insertion's own coincident cell, kappa=1).
                    if !(s_i == s_y && a_i == a_y) {
                        n_related += 1;
                        min_related_mass = min_related_mass.min(m_i);
                    }
                }
                // A candidate unrelated to all existing mass (e.g. an
                // ancestor whose k_src level to its descendants is 0)
                // has C_y = 0: gain is +inf, nothing eroded, trivially
                // safe. Skip so it doesn't dominate the "safe" tail.
                if c_y == 0.0 {
                    n_candidates -= 1;
                    continue;
                }
                loss *= 1.0 - qd;
                let gain = c_y.powf(qd - 1.0);
                let marginal = gain - loss;
                let ratio = if loss > 0.0 {
                    gain / loss
                } else {
                    f64::INFINITY
                };
                if n_related > 0 {
                    n_bridging += 1;
                }
                if marginal < 0.0 {
                    n_violations += 1;
                }
                all.push(InsertionMargin {
                    entry: entry.clone(),
                    source: s_y.clone(),
                    anchor: a_y.clone(),
                    gain,
                    loss,
                    marginal,
                    ratio,
                    c_y,
                    n_related,
                    min_related_mass: if min_related_mass.is_finite() {
                        min_related_mass
                    } else {
                        0.0
                    },
                });
            }
        }
    }

    // total_cmp gives a defined order even if any ratio is NaN, so the
    // true minimum is never lost behind an unordered NaN.
    all.sort_by(|x, y| x.ratio.total_cmp(&y.ratio));
    let worst_ratio = all.first().map_or(f64::INFINITY, |m| m.ratio);
    all.truncate(keep);

    AuditReport {
        q,
        n_entries_scored,
        n_candidates,
        n_bridging,
        n_violations,
        worst_ratio,
        worst: all,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Entry, Homogeneity, Impact, ImpactId, Isotonic, Palette, RelationScaling};

    fn params(q: f32) -> Params {
        Params {
            q,
            k_src_levels: KSrcLevels {
                levels: [0.95, 0.5, 0.3, 0.15, 0.0],
            },
            k_feat: KFeatMatrix::default(),
            homogeneity: Homogeneity::DegreeQ,
            calibration: Isotonic::default(),
            relation_scaling: RelationScaling::Fixed,
        }
    }

    fn entry(id: &str) -> Entry {
        Entry {
            id: EntryId(id.into()),
            store: "s".into(),
            store_id: id.into(),
            title_cache: None,
        }
    }

    /// A single isolated atom can never be a harm zone: adding more of
    /// the *only* thing present just intensifies it, nothing to erode.
    #[test]
    fn lone_atom_is_safe() {
        let snap = Snapshot {
            entries: vec![entry("e")],
            impacts: vec![Impact {
                id: ImpactId("i".into()),
                target: EntryId("e".into()),
                atoms: vec![crate::types::Atom {
                    source: SourceId("s".into()),
                    blend: vec![(AnchorId("a".into()), 1.0)],
                    magnitude: 3.0,
                    time: crate::types::TimeSupport::Moment,
                }],
            }],
            relations: vec![],
            palette: Palette {
                anchors: vec![],
                k_feat: KFeatMatrix::default(),
            },
            taxonomy: Taxonomy::default(),
        };
        let report = audit_monotonicity(&snap, &params(0.1), 10);
        assert_eq!(report.n_violations, 0);
        // Only self-erosion (kappa=1) of a single cell; ratio stays >=1.
        assert!(report.worst_ratio >= 1.0, "{}", report.worst_ratio);
    }

    /// Reproduce the §1.2 harm-zone shape within the live kernel: N
    /// small atoms at one anchor, distinct sources that are mutually
    /// **isolated** (`k_src` at their pairwise distance = 0) but each
    /// related to a common parent source. Inserting at the *parent*
    /// (a bridge point, distance 1 = a high k_src level) erodes all N
    /// otherwise-isolated small atoms at once — the §1.2 mechanism. The
    /// audit must include the parent as a candidate to catch it.
    #[test]
    fn many_small_related_atoms_flag_a_violation_at_low_q() {
        let n = 12;
        let mut entries = vec![entry("e")];
        entries.truncate(1);
        let mut impacts = Vec::new();
        let mut taxonomy = Taxonomy::default();
        let parent = SourceId("franchise".into());
        for i in 0..n {
            let s = SourceId(format!("child{i}"));
            taxonomy.parent.insert(s.clone(), parent.clone());
            impacts.push(Impact {
                id: ImpactId(format!("i{i}")),
                target: EntryId("e".into()),
                atoms: vec![crate::types::Atom {
                    source: s,
                    blend: vec![(AnchorId("a".into()), 1.0)],
                    magnitude: 0.02,
                    time: crate::types::TimeSupport::Moment,
                }],
            });
        }
        let snap = Snapshot {
            entries,
            impacts,
            relations: vec![],
            palette: Palette {
                anchors: vec![],
                k_feat: KFeatMatrix::default(),
            },
            taxonomy,
        };
        // Distance 1 (child<->parent) = index 0 = 0.95 (strong bridge);
        // distance 2 (child<->child) = index 1 = 0.0 (mutually isolated).
        // So each small atom is near-isolated (crowding ≈ its own mass)
        // yet the parent bridges all of them.
        let mut p = params(0.1);
        p.k_src_levels = KSrcLevels {
            levels: [0.95, 0.0, 0.0, 0.0, 0.0],
        };
        let report = audit_monotonicity(&snap, &p, 20);
        assert!(
            report.n_violations > 0,
            "expected a harm zone; worst_ratio={}",
            report.worst_ratio
        );
        assert!(report.worst_ratio < 1.0);
    }
}
