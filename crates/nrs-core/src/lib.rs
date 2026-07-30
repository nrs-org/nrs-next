//! The pure scoring engine (INFRA.md §4). I/O-free and deterministic:
//! `score`/`explain` are the entire contract, everything else is the
//! plain data they take and return.

mod measure;
mod redundancy;
mod solve;
pub mod types;

pub use types::*;

fn require_degree_q(homogeneity: Homogeneity) {
    // The degree-1 alternate form (notes ch. "The value kernel, up
    // close" §"Two knobs, and two honest subtleties") is sketched but
    // not pinned down there (an unspecified outer discount `g`); every
    // proof this engine is tested against (splittability, the
    // double-count bound) is stated in terms of the degree-q default,
    // so only that form is implemented.
    if homogeneity != Homogeneity::DegreeQ {
        todo!("Homogeneity::Degree1 is underspecified in the notes (§7.2); not implemented")
    }
}

/// INFRA.md §4.3, phases 1-4: build measures, solve relations, apply the
/// one nonlinearity, calibrate.
pub fn score(snap: &Snapshot, params: &Params) -> Scores {
    require_degree_q(params.homogeneity);
    let built = measure::build_measures(snap);
    let resolved = solve::solve_relations(built, &snap.relations);

    let mut values = std::collections::BTreeMap::new();
    for (entry, f) in &resolved {
        let (v, _) = redundancy::value(
            f,
            &params.k_feat,
            &snap.taxonomy,
            &params.k_src_levels,
            params.q,
        );
        values.insert(entry.clone(), params.calibration.apply(v));
    }
    Scores { values }
}

/// Same contract as `score`, but returns one entry's per-`(source,
/// anchor)` breakdown (INFRA.md §4.3 phase 3).
pub fn explain(snap: &Snapshot, params: &Params, entry: EntryId) -> Explanation {
    require_degree_q(params.homogeneity);
    let built = measure::build_measures(snap);
    let resolved = solve::solve_relations(built, &snap.relations);

    let empty = measure::Measure::new();
    let f = resolved.get(&entry).unwrap_or(&empty);
    let (value, contributions) = redundancy::value(
        f,
        &params.k_feat,
        &snap.taxonomy,
        &params.k_src_levels,
        params.q,
    );
    let score = params.calibration.apply(value);
    Explanation {
        entry,
        value,
        score,
        contributions,
    }
}
