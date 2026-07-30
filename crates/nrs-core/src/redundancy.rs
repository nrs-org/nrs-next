//! Phase 3 (INFRA.md §4.3, notes ch. "The value kernel, up close"):
//! the one nonlinearity. Per entry, given its fully-resolved measure
//! `f`, crowding is `c = K * f` with `K = k_src . k_feat` (`k_time`
//! fixed at 1, notes §"What we integrate over"), and the entry's value
//! is `V = sum f(x) . phi(c(x))`, `phi(c) = c^(q-1)` (Def. "the
//! redundancy exponent q").
//!
//! The notes' worked examples are all same-signed masses; crowding here
//! sums `|mass|` rather than signed mass so that `phi`'s fractional
//! power is always applied to a nonnegative number (avoiding complex
//! results), while the outer product `f(x) . phi(c(x))` keeps the
//! cell's own sign — a negative impact still discounts by local
//! crowding like everything else, it just contributes negatively.

use crate::measure::Measure;
use crate::types::{Contribution, KFeatMatrix, KSrcLevels, SourceId, Taxonomy};

fn ancestor_chain(taxonomy: &Taxonomy, id: &SourceId) -> Vec<SourceId> {
    let mut chain = vec![id.clone()];
    let mut current = id.clone();
    while let Some(parent) = taxonomy.parent.get(&current) {
        if chain.contains(parent) {
            break;
        }
        chain.push(parent.clone());
        current = parent.clone();
    }
    chain
}

/// `k_src`: 1.0 at an identical source; otherwise the level indexed by
/// the combined tree-distance to the deepest shared ancestor (notes
/// table: same character/scene, same work, same franchise, same
/// genre/vibe, unrelated), falling back to the "unrelated" level when
/// the two sources share no ancestor at all. The exact distance-to-level
/// mapping is a modelling choice pending `nrs-fit` (roadmap phase 6);
/// this only needs to be monotone in "how far apart".
fn k_src(a: &SourceId, b: &SourceId, taxonomy: &Taxonomy, levels: &KSrcLevels) -> f32 {
    if a == b {
        return 1.0;
    }
    let chain_a = ancestor_chain(taxonomy, a);
    let chain_b = ancestor_chain(taxonomy, b);
    for (da, node) in chain_a.iter().enumerate() {
        if let Some(db) = chain_b.iter().position(|n| n == node) {
            let dist = da + db;
            let idx = dist.saturating_sub(1).min(levels.levels.len() - 1);
            return levels.levels[idx];
        }
    }
    *levels.levels.last().unwrap()
}

/// `V(f) = sum_x f(x) . phi(c(x))`, and the per-cell breakdown behind
/// it (source, anchor, mass, crowding, contribution).
pub(crate) fn value(
    f: &Measure,
    k_feat: &KFeatMatrix,
    taxonomy: &Taxonomy,
    k_src_levels: &KSrcLevels,
    q: f32,
) -> (f32, Vec<Contribution>) {
    let cells: Vec<(&SourceId, &crate::types::AnchorId, f32)> = f
        .iter()
        .filter(|(_, m)| **m != 0.0)
        .map(|((s, a), m)| (s, a, *m))
        .collect();

    let mut total = 0.0;
    let mut contributions = Vec::with_capacity(cells.len());
    for &(source_i, anchor_i, mass_i) in &cells {
        let mut crowding = 0.0;
        for &(source_j, anchor_j, mass_j) in &cells {
            let k =
                k_feat.get(anchor_i, anchor_j) * k_src(source_i, source_j, taxonomy, k_src_levels);
            crowding += k * mass_j.abs();
        }
        let phi = crowding.powf(q - 1.0);
        let contribution = mass_i * phi;
        total += contribution;
        contributions.push(Contribution {
            source: source_i.clone(),
            anchor: anchor_i.clone(),
            mass: mass_i,
            crowding,
            contribution,
        });
    }
    (total, contributions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AnchorId;
    use std::collections::BTreeMap;

    #[test]
    fn identical_source_is_maximally_related() {
        let s = SourceId("same".into());
        let tax = Taxonomy::default();
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        assert_eq!(k_src(&s, &s, &tax, &levels), 1.0);
    }

    #[test]
    fn unrelated_sources_use_last_level() {
        let a = SourceId("a".into());
        let b = SourceId("b".into());
        let tax = Taxonomy::default();
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        assert_eq!(k_src(&a, &b, &tax, &levels), 0.0);
    }

    #[test]
    fn same_feeling_twice_saturates_below_two_distinct() {
        // Notes scenario ("same feeling twice vs. two distinct
        // feelings"): two feelings of size `a`, q = 0.5. Same
        // (source, anchor) twice: V = sqrt(2a) * ... ; two distinct
        // cells: V = 2*sqrt(a). The former must be strictly smaller.
        let a = 3.0f32;
        let q = 0.5;
        let k_feat = KFeatMatrix::default();
        let tax = Taxonomy::default();
        // Unrelated sources (no shared ancestor) fall back to the last
        // level, so two distinct sources here are ~fully unredundant.
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        let anchor = AnchorId("x".into());
        let source = SourceId("s".into());

        let mut same_cell = Measure::new();
        *same_cell
            .entry((source.clone(), anchor.clone()))
            .or_insert(0.0) += a;
        *same_cell
            .entry((source.clone(), anchor.clone()))
            .or_insert(0.0) += a;
        let (v_same, _) = value(&same_cell, &k_feat, &tax, &levels, q);

        let mut distinct_cells: BTreeMap<_, _> = BTreeMap::new();
        distinct_cells.insert((SourceId("s1".into()), anchor.clone()), a);
        distinct_cells.insert((SourceId("s2".into()), anchor.clone()), a);
        let (v_distinct, _) = value(&distinct_cells, &k_feat, &tax, &levels, q);

        assert!(v_same < v_distinct, "{v_same} should be < {v_distinct}");
    }
}
