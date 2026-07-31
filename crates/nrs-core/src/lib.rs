//! The pure scoring engine (INFRA.md §4). I/O-free and deterministic:
//! `score`/`explain` are the entire contract, everything else is the
//! plain data they take and return.

mod measure;
mod redundancy;
mod solve;
pub mod types;

pub use types::*;

/// INFRA.md §4.3, phases 1-4: build measures, solve relations, apply the
/// one nonlinearity, calibrate.
pub fn score(snap: &Snapshot, params: &Params) -> Scores {
    let built = measure::build_measures(snap);
    let resolved =
        solve::solve_relations(built, &snap.relations, params.q, params.relation_scaling);

    let mut values = std::collections::BTreeMap::new();
    for (entry, f) in &resolved {
        let (v, _) = redundancy::value(
            f,
            &params.k_feat,
            &snap.taxonomy,
            &params.k_src_levels,
            params.q,
            params.homogeneity,
        );
        values.insert(entry.clone(), params.calibration.apply(v));
    }
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
