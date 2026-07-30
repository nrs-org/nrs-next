//! The pure scoring engine (INFRA.md §4). I/O-free and deterministic:
//! `score`/`explain` are the entire contract, everything else is the
//! plain data they take and return.

pub mod types;

pub use types::*;

/// INFRA.md §4.3, phases 1-4: build measures, solve relations, apply the
/// one nonlinearity, calibrate. Not yet implemented (roadmap phase 2).
pub fn score(_snap: &Snapshot, _params: &Params) -> Scores {
    todo!("nrs-core phase 2: build measures -> solve relations -> V(f) -> calibrate")
}

/// Same contract as `score`, but returns one entry's per-`(source, anchor)`
/// breakdown (INFRA.md §4.3 phase 3). Not yet implemented.
pub fn explain(_snap: &Snapshot, _params: &Params, _entry: EntryId) -> Explanation {
    todo!("nrs-core phase 2: per-(source, anchor) contribution breakdown")
}
