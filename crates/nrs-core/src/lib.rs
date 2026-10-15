//! The pure scoring engine (INFRA.md §4). I/O-free and deterministic:
//! `score`/`explain` are the entire contract, everything else is the
//! plain data they take and return.
//!
//! Pipeline: each impact embeds as `e = sgnpow(m, 1/w_anchor)` per
//! anchor (phase 1); relations move capped fractions of substance, linear
//! in `e` (phase 2); per entry, anchors pool by nested signed `l_p` over
//! the single-linkage tree of `K` (phase 3); calibration maps the value
//! to the public scale (phase 4). Under `SignMode::Split` phases 1-3 run
//! once on the positive atoms and once on the negative ones, and the
//! value is their difference.

mod measure;
mod pool;
mod solve;
pub mod types;

use std::collections::BTreeMap;

use rayon::prelude::*;

use measure::Part;
pub use pool::{apply_corrections, ultrametric_closure, KCorrection};
pub use types::*;

/// INFRA.md §4.3, phases 1-4. Entries score independently once their
/// measures are resolved, so phase 3 fans out over `rayon`.
pub fn score(snap: &Snapshot, params: &Params) -> Scores {
    let kernel = pool::Kernel::new(&params.k_feat);
    let mut values: BTreeMap<EntryId, f32> = BTreeMap::new();
    for &(part, sign) in Part::of(params.sign) {
        let built = measure::build_measures(snap, &kernel, part);
        let resolved = solve::solve_relations(built, &snap.relations);

        // Collected to a `Vec` first: rayon splits a slice for work-stealing
        // more cheaply than `BTreeMap`'s tree-shaped parallel iterator.
        let entries: Vec<_> = resolved.iter().collect();
        let part_values: Vec<(EntryId, f32)> = entries
            .par_iter()
            .map(|&(entry, f)| {
                let (v, _) = pool::value(f, &kernel, &params.time);
                (entry.clone(), v)
            })
            .collect();
        for (entry, v) in part_values {
            *values.entry(entry).or_insert(0.0) += sign as f32 * v;
        }
    }
    let values = values
        .into_iter()
        .map(|(entry, v)| (entry, params.calibration.apply(v)))
        .collect();
    Scores { values }
}

/// Same contract as `score`, but returns one entry's per-anchor
/// breakdown (INFRA.md §4.3 phase 3), and each anchor's share split
/// across the impacts whose mass reaches it (phase 2 in reverse). Under
/// `SignMode::Split` an anchor's or impact's row nets its positive part
/// against its negative one.
pub fn explain(snap: &Snapshot, params: &Params, entry: EntryId) -> Explanation {
    let kernel = pool::Kernel::new(&params.k_feat);
    let mut value = 0.0f32;
    let mut contributions: BTreeMap<AnchorId, Contribution> = BTreeMap::new();
    let mut sources: BTreeMap<(AnchorId, EntryId, AnchorId, ImpactId), Source> = BTreeMap::new();
    for &(part, sign) in Part::of(params.sign) {
        let sign = sign as f32;
        let built = measure::build_measures(snap, &kernel, part);
        let borrow = solve::borrow(built, &snap.relations);
        let resolved = borrow.resolved();

        let empty = measure::Measure::new();
        let f = resolved.get(&entry).unwrap_or(&empty);
        let (v, part_contributions) = pool::value(f, &kernel, &params.time);
        value += sign * v;
        for s in part_sources(snap, &kernel, part, &borrow, &entry, &part_contributions) {
            let key = (
                s.anchor.clone(),
                s.entry.clone(),
                s.origin_anchor.clone(),
                s.impact.clone(),
            );
            let row = sources.entry(key).or_insert(Source {
                embedded: 0.0,
                contribution: 0.0,
                ..s.clone()
            });
            row.embedded += sign * s.embedded;
            row.contribution += sign * s.contribution;
        }
        for c in part_contributions {
            let row = contributions
                .entry(c.anchor.clone())
                .or_insert(Contribution {
                    embedded: 0.0,
                    mass: 0.0,
                    contribution: 0.0,
                    ..c.clone()
                });
            row.embedded += sign * c.embedded;
            row.mass += sign * c.mass;
            row.contribution += sign * c.contribution;
        }
    }
    Explanation {
        entry,
        value,
        score: params.calibration.apply(value),
        contributions: contributions.into_values().collect(),
        sources: sources.into_values().collect(),
    }
}

/// Embedded mass is linear in `phi` and in impacts, so each anchor's
/// mass in `part` splits exactly into impact pieces; its contribution is
/// split in the same proportions, as `pool::value` already splits a box.
fn part_sources(
    snap: &Snapshot,
    kernel: &pool::Kernel,
    part: Part,
    borrow: &solve::Borrow,
    entry: &EntryId,
    contributions: &[Contribution],
) -> Vec<Source> {
    let per_impact = measure::impact_embeds(snap, kernel, part);
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
