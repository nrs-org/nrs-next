//! Phase 1 (INFRA.md §4.3): fold every impact's atoms into each target
//! entry's own measure. Pure addition — splittable (chopping a tail's
//! time-support doesn't change its integrated area) and associative.

use std::collections::BTreeMap;

use crate::types::{Atom, EntryId, Snapshot, TimeSupport};

/// Sparse per-entry measure: mass at each `(source, anchor)` cell
/// (INFRA.md §4.3 phase 1's "sparse map (SourceId, AnchorId) -> f32").
pub(crate) type Measure = BTreeMap<(crate::types::SourceId, crate::types::AnchorId), f32>;

/// The area under a time-support's intensity (notes ch. "Time: value as
/// an integral"): a moment is mass `m` at an instant (area = 1, i.e. the
/// atom's own magnitude passes through unscaled); a tail integrates
/// `peak * exp(-lambda * t)` from `0` to `cutoff` — an infinite cutoff
/// recovers the untruncated `peak / lambda`.
fn life(time: &TimeSupport) -> f32 {
    match *time {
        TimeSupport::Moment => 1.0,
        TimeSupport::Tail {
            peak,
            lambda,
            cutoff,
        } => peak * (1.0 - (-lambda * cutoff).exp()) / lambda,
    }
}

fn atom_mass(atom: &Atom) -> f32 {
    atom.magnitude * life(&atom.time)
}

/// Build every entry's own measure from its impacts' atoms, before any
/// relation is applied. An atom's mass splits across its blend anchors
/// in proportion to the blend weights.
pub(crate) fn build_measures(snap: &Snapshot) -> BTreeMap<EntryId, Measure> {
    let mut measures: BTreeMap<EntryId, Measure> = BTreeMap::new();
    for entry in &snap.entries {
        measures.entry(entry.id.clone()).or_default();
    }
    for impact in &snap.impacts {
        let measure = measures.entry(impact.target.clone()).or_default();
        for atom in &impact.atoms {
            let mass = atom_mass(atom);
            for (anchor, frac) in &atom.blend {
                *measure
                    .entry((atom.source.clone(), anchor.clone()))
                    .or_insert(0.0) += mass * frac;
            }
        }
    }
    measures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    #[test]
    fn moment_mass_is_plain_magnitude() {
        let atom = Atom {
            source: SourceId("s".into()),
            blend: vec![(AnchorId("a".into()), 1.0)],
            magnitude: 3.0,
            time: TimeSupport::Moment,
        };
        assert_eq!(atom_mass(&atom), 3.0);
    }

    #[test]
    fn splitting_a_tail_preserves_total_area() {
        // Splittability (P6): chopping a tail at any point and scoring
        // the two pieces separately must sum to the whole (Prop.
        // "splittability => additivity in time").
        let peak = 2.0;
        let lambda = 0.5;
        let cutoff = 4.0;
        let whole = life(&TimeSupport::Tail {
            peak,
            lambda,
            cutoff,
        });

        let split_at = 1.7;
        let first = peak * (1.0 - (-lambda * split_at).exp()) / lambda;
        // The remainder is the same decaying tail, re-based at t=0 with
        // a lower peak (its value at the cut point) and the leftover
        // duration.
        let remainder_peak = peak * (-lambda * split_at).exp();
        let second = remainder_peak * (1.0 - (-lambda * (cutoff - split_at)).exp()) / lambda;

        assert!((whole - (first + second)).abs() < 1e-5);
    }
}
