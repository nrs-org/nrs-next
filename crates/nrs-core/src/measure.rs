//! Phase 1 (INFRA.md §4.3): fold every impact's atoms into each target
//! entry's own measure.
//!
//! The measure is one signed **embedded** mass per anchor. A piece of
//! value-unit mass `m` on anchor `a` embeds as `e = sgnpow(m, 1/w_a)`
//! (`w_a = K(a, a)`), and embedded masses add. This is the `l_(1/w)`
//! pooling of repeated value on one anchor, kept linear so that
//! grouping never changes the answer: splitting an impact into pieces
//! with the same features, or cancelling a positive against a negative,
//! lands on the same `e` however the pieces are grouped. No impact or
//! source identity survives into the measure.

use std::collections::BTreeMap;

use crate::pool::Kernel;
use crate::types::{AnchorId, Atom, EntryId, Snapshot, TimeSupport};

/// Sparse per-entry measure: signed embedded mass per anchor.
pub(crate) type Measure = BTreeMap<AnchorId, f64>;

/// `sgn(x) |x|^p`, the signed power that keeps embedding and reading
/// back odd (so opposite masses cancel exactly in embedded units).
pub(crate) fn sgnpow(x: f64, p: f64) -> f64 {
    x.signum() * x.abs().powf(p)
}

/// The area under a time-support's intensity: a moment passes its
/// magnitude through unscaled; a tail integrates `peak * exp(-lambda *
/// t)` from `0` to `cutoff`. A tail is still one atom: its total enters
/// as a single value-unit mass (time is not a feature yet).
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

/// Build every entry's own embedded measure from its impacts' atoms,
/// before any relation is applied. An atom's mass splits across its
/// blend anchors in proportion to the blend weights, and each share
/// embeds with its own anchor's exponent.
pub(crate) fn build_measures(snap: &Snapshot, kernel: &Kernel) -> BTreeMap<EntryId, Measure> {
    let mut measures: BTreeMap<EntryId, Measure> = BTreeMap::new();
    for entry in &snap.entries {
        measures.entry(entry.id.clone()).or_default();
    }
    for impact in &snap.impacts {
        let measure = measures.entry(impact.target.clone()).or_default();
        for atom in &impact.atoms {
            let mass = f64::from(atom_mass(atom));
            for (anchor, frac) in &atom.blend {
                let share = mass * f64::from(*frac);
                if share == 0.0 {
                    continue;
                }
                let w = kernel.exponent_of(anchor);
                *measure.entry(anchor.clone()).or_insert(0.0) += sgnpow(share, 1.0 / w);
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
    fn sgnpow_is_odd() {
        assert_eq!(sgnpow(-8.0, 1.0 / 3.0), -sgnpow(8.0, 1.0 / 3.0));
        assert_eq!(sgnpow(0.0, 2.5), 0.0);
    }
}
