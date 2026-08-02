//! The pure scoring engine (INFRA.md §4). I/O-free and deterministic:
//! `score`/`explain` are the entire contract, everything else is the
//! plain data they take and return.

mod audit;
mod linalg;
mod measure;
mod redundancy;
mod solve;
pub mod types;

use rayon::prelude::*;

pub use audit::{audit_monotonicity, AuditReport, InsertionMargin};
pub use types::*;

/// INFRA.md §4.3, phases 1-4: build measures, solve relations, apply the
/// one nonlinearity, calibrate.
///
/// Entries score independently of one another (phase 3 only reads one
/// entry's own resolved measure), so this fans the per-entry work out
/// over `rayon`'s thread pool. Each task gets its own `SourceIndex`
/// rather than sharing one: cross-entry chain memoization no longer
/// pays for itself now that `redundancy::bucket_crowding` has cut most
/// of the pairwise `k_src` traffic it existed to amortize, and a fresh
/// per-task index avoids synchronizing mutable state across threads.
pub fn score(snap: &Snapshot, params: &Params) -> Scores {
    let built = measure::build_measures(snap);
    let resolved =
        solve::solve_relations(built, &snap.relations, params.q, params.relation_scaling);

    // Collected to a `Vec` first: rayon splits a slice for work-stealing
    // more cheaply than `BTreeMap`'s tree-shaped parallel iterator.
    let entries: Vec<_> = resolved.iter().collect();
    let values: std::collections::BTreeMap<EntryId, f32> = entries
        .par_iter()
        .map(|&(entry, f)| {
            let mut src_index = redundancy::SourceIndex::new();
            let (v, _) = redundancy::value(
                f,
                &params.k_feat,
                &snap.taxonomy,
                &mut src_index,
                &params.k_src_levels,
                params.q,
                params.homogeneity,
            );
            (entry.clone(), params.calibration.apply(v))
        })
        .collect();
    Scores { values }
}

/// Same contract as `score`, but returns one entry's per-`(source,
/// anchor)` breakdown (INFRA.md §4.3 phase 3).
pub fn explain(snap: &Snapshot, params: &Params, entry: EntryId) -> Explanation {
    let built = measure::build_measures(snap);
    let resolved =
        solve::solve_relations(built, &snap.relations, params.q, params.relation_scaling);

    let empty = measure::Measure::new();
    let f = resolved.get(&entry).unwrap_or(&empty);
    let (value, contributions) = redundancy::value(
        f,
        &params.k_feat,
        &snap.taxonomy,
        &mut redundancy::SourceIndex::new(),
        &params.k_src_levels,
        params.q,
        params.homogeneity,
    );
    let score = params.calibration.apply(value);
    Explanation {
        entry,
        value,
        score,
        contributions,
    }
}
