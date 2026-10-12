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
/// breakdown (INFRA.md §4.3 phase 3).
pub fn explain(snap: &Snapshot, params: &Params, entry: EntryId) -> Explanation {
    let kernel = pool::Kernel::new(&params.k_feat);
    let built = measure::build_measures(snap, &kernel);
    let resolved = solve::solve_relations(built, &snap.relations);

    let empty = measure::Measure::new();
    let f = resolved.get(&entry).unwrap_or(&empty);
    let (value, contributions) = pool::value(f, &kernel, &params.time);
    let score = params.calibration.apply(value);
    Explanation {
        entry,
        value,
        score,
        contributions,
    }
}
