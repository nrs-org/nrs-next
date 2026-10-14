//! The pure scoring engine (INFRA.md §4). I/O-free and deterministic:
//! `score`/`explain` are the entire contract, everything else is the
//! plain data they take and return.
//!
//! Pipeline: each impact embeds as signed `e = sgnpow(m, 1/w_anchor)` per
//! anchor (phase 1); relations move capped fractions of substance, linear
//! in `e` (phase 2); per entry, anchors pool by nested signed `l_p` over
//! the single-linkage tree of `K` (phase 3); calibration maps the value
//! to the public scale (phase 4).

mod measure;
mod pool;
mod solve;
pub mod types;

use rayon::prelude::*;

pub use pool::{apply_corrections, ultrametric_closure, KCorrection};
pub use types::*;

/// INFRA.md §4.3, phases 1-4. Entries score independently once their
/// measures are resolved, so phase 3 fans out over `rayon`.
pub fn score(snap: &Snapshot, params: &Params) -> Scores {
    let kernel = pool::Kernel::new(&params.k_feat);
    let built = measure::build_measures(snap, &kernel);
    let resolved = solve::solve_relations(built, &snap.relations);

    // Collected to a `Vec` first: rayon splits a slice for work-stealing
    // more cheaply than `BTreeMap`'s tree-shaped parallel iterator.
    let entries: Vec<_> = resolved.iter().collect();
    let values: std::collections::BTreeMap<EntryId, f32> = entries
        .par_iter()
        .map(|&(entry, f)| {
            let (v, _) = pool::value(f, &kernel, &params.time);
            (entry.clone(), params.calibration.apply(v))
        })
        .collect();
    Scores { values }
}

/// Same contract as `score`, but returns one entry's per-anchor
/// breakdown (INFRA.md §4.3 phase 3), and each anchor's share split
/// across the impacts whose mass reaches it (phase 2 in reverse).
pub fn explain(snap: &Snapshot, params: &Params, entry: EntryId) -> Explanation {
    let kernel = pool::Kernel::new(&params.k_feat);
    let built = measure::build_measures(snap, &kernel);
    let borrow = solve::borrow(built, &snap.relations);
    let resolved = borrow.resolved();

    let empty = measure::Measure::new();
    let f = resolved.get(&entry).unwrap_or(&empty);
    let (value, contributions) = pool::value(f, &kernel, &params.time);
    let score = params.calibration.apply(value);
    let sources = sources(snap, &kernel, &borrow, &entry, &contributions);
    Explanation {
        entry,
        value,
        score,
        contributions,
        sources,
    }
}

/// Embedded mass is linear in `phi` and in impacts, so each anchor's
/// mass splits exactly into impact pieces; its contribution is split in
/// the same proportions, as `pool::value` already splits a box.
fn sources(
    snap: &Snapshot,
    kernel: &pool::Kernel,
    borrow: &solve::Borrow,
    entry: &EntryId,
    contributions: &[Contribution],
) -> Vec<Source> {
    use std::collections::BTreeMap;

    let per_impact = measure::impact_embeds(snap, kernel);
    let mut embedded: BTreeMap<(&AnchorId, &EntryId, &AnchorId, &ImpactId), f64> = BTreeMap::new();
    for (anchor, origin, origin_anchor, phi) in borrow.origins(entry) {
        let Some(impacts) = per_impact.get(&(origin.clone(), origin_anchor.clone())) else {
            continue;
        };
        for (impact, e) in impacts {
            *embedded
                .entry((anchor, origin, origin_anchor, impact))
                .or_insert(0.0) += phi * e;
        }
    }
    let anchor_share: BTreeMap<&AnchorId, f64> = contributions
        .iter()
        .filter(|c| c.embedded != 0.0)
        .map(|c| (&c.anchor, f64::from(c.contribution) / f64::from(c.embedded)))
        .collect();
    embedded
        .into_iter()
        .filter(|(_, e)| *e != 0.0)
        .map(|((anchor, origin, origin_anchor, impact), e)| Source {
            anchor: anchor.clone(),
            entry: origin.clone(),
            origin_anchor: origin_anchor.clone(),
            impact: impact.clone(),
            embedded: e as f32,
            contribution: (e * anchor_share.get(anchor).copied().unwrap_or(0.0)) as f32,
        })
        .collect()
}
