//! Phase 1 (INFRA.md §4.3): fold every impact's atoms into each target
//! entry's own measure.
//!
//! The measure is signed **embedded** mass per anchor, undated or spread
//! over dated spans. A piece of value-unit mass `m` on anchor `a` embeds
//! as `e = sgnpow(m, 1/w_a)` (`w_a = K(a, a)`), and embedded masses add.
//! This is the `l_(1/w)` pooling of repeated value on one anchor, kept
//! linear so that grouping never changes the answer: splitting an impact
//! into pieces with the same features lands on the same `e` however the
//! pieces are grouped. No impact or source identity survives into the
//! measure.
//!
//! A measure holds one `Part` of the atoms: under `SignMode::Split` the
//! positive and negative shares build separate measures (the negative one
//! with magnitudes flipped positive); under `SignMode::Signed` one measure
//! holds both, and opposite masses cancel in embedded units.

use std::collections::BTreeMap;

use crate::pool::Kernel;
use crate::types::{AnchorId, Atom, EntryId, ImpactId, SignMode, Snapshot, TimeSupport};

/// Which atom shares one measure holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    /// Every share, signed.
    All,
    /// Positive shares only.
    Positive,
    /// Negative shares only, flipped positive.
    Negative,
}

impl Part {
    /// The measures a sign mode scores, each with the sign its value
    /// carries in the entry's total.
    pub(crate) fn of(mode: SignMode) -> &'static [(Part, f64)] {
        match mode {
            SignMode::Signed => &[(Part::All, 1.0)],
            SignMode::Split => &[(Part::Positive, 1.0), (Part::Negative, -1.0)],
        }
    }

    /// `share` as this part holds it, or `None` if it belongs elsewhere.
    fn take(self, share: f64) -> Option<f64> {
        match self {
            Part::All => Some(share),
            Part::Positive => (share > 0.0).then_some(share),
            Part::Negative => (share < 0.0).then_some(-share),
        }
    }
}

/// Embedded mass `e` spread evenly over days `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Span {
    pub from: f64,
    pub to: f64,
    pub e: f64,
}

/// Sparse per-entry measure: signed embedded mass per anchor, undated
/// and dated.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Measure {
    pub undated: BTreeMap<AnchorId, f64>,
    pub dated: BTreeMap<AnchorId, Vec<Span>>,
}

impl Measure {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Adds `span`, folding it into an existing span over the same days.
    pub(crate) fn add_span(&mut self, anchor: &AnchorId, span: Span) {
        let spans = self.dated.entry(anchor.clone()).or_default();
        match spans
            .iter_mut()
            .find(|s| s.from == span.from && s.to == span.to)
        {
            Some(existing) => existing.e += span.e,
            None => spans.push(span),
        }
    }
}

/// `sgn(x) |x|^p`, the signed power that keeps embedding and reading
/// back odd (so opposite masses cancel exactly in embedded units).
pub(crate) fn sgnpow(x: f64, p: f64) -> f64 {
    x.signum() * x.abs().powf(p)
}

/// The area under a time-support's intensity: a moment or a dated
/// period passes its magnitude through unscaled; a tail integrates
/// `peak * exp(-lambda * t)` from `0` to `cutoff`.
fn life(time: &TimeSupport) -> f32 {
    match *time {
        TimeSupport::Moment | TimeSupport::Interval { .. } => 1.0,
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

/// One atom's embedded pieces in `part`: per blend anchor, its share's
/// embedded mass `e` and, for a dated atom, the days `[from, to)` it
/// spreads over.
fn atom_embeds<'a>(
    atom: &'a Atom,
    kernel: &'a Kernel,
    part: Part,
) -> impl Iterator<Item = (&'a AnchorId, f64, Option<(f64, f64)>)> + 'a {
    let mass = f64::from(atom_mass(atom));
    atom.blend.iter().filter_map(move |(anchor, frac)| {
        let share = mass * f64::from(*frac);
        if share == 0.0 {
            return None;
        }
        let share = part.take(share)?;
        let e = sgnpow(share, 1.0 / kernel.exponent_of(anchor));
        let days = match atom.time {
            TimeSupport::Interval { from, to } => {
                let from = f64::from(from);
                Some((from, f64::from(to).max(from + 1.0)))
            }
            _ => None,
        };
        Some((anchor, e, days))
    })
}

/// Build every entry's own embedded measure of `part` from its impacts'
/// atoms, before any relation is applied. An atom's mass splits across
/// its blend anchors in proportion to the blend weights, and each share
/// embeds with its own anchor's exponent.
pub(crate) fn build_measures(
    snap: &Snapshot,
    kernel: &Kernel,
    part: Part,
) -> BTreeMap<EntryId, Measure> {
    let mut measures: BTreeMap<EntryId, Measure> = BTreeMap::new();
    for entry in &snap.entries {
        measures.entry(entry.id.clone()).or_default();
    }
    for impact in &snap.impacts {
        let measure = measures.entry(impact.target.clone()).or_default();
        for atom in &impact.atoms {
            for (anchor, e, days) in atom_embeds(atom, kernel, part) {
                match days {
                    Some((from, to)) => measure.add_span(anchor, Span { from, to, e }),
                    None => *measure.undated.entry(anchor.clone()).or_insert(0.0) += e,
                }
            }
        }
    }
    measures
}

/// Each impact's own embedded mass per `(target, anchor)`, undated and
/// dated together: the pieces `build_measures` sums, kept apart.
pub(crate) fn impact_embeds(
    snap: &Snapshot,
    kernel: &Kernel,
    part: Part,
) -> BTreeMap<(EntryId, AnchorId), BTreeMap<ImpactId, f64>> {
    let mut out: BTreeMap<(EntryId, AnchorId), BTreeMap<ImpactId, f64>> = BTreeMap::new();
    for impact in &snap.impacts {
        for atom in &impact.atoms {
            for (anchor, e, _) in atom_embeds(atom, kernel, part) {
                *out.entry((impact.target.clone(), anchor.clone()))
                    .or_default()
                    .entry(impact.id.clone())
                    .or_insert(0.0) += e;
            }
        }
    }
    out
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

    #[test]
    fn spans_over_the_same_days_fold_together() {
        let mut m = Measure::new();
        let a = AnchorId("a".into());
        m.add_span(&a, Span { from: 0.0, to: 10.0, e: 1.0 });
        m.add_span(&a, Span { from: 0.0, to: 10.0, e: 2.0 });
        m.add_span(&a, Span { from: 5.0, to: 10.0, e: 1.0 });
        assert_eq!(m.dated[&a].len(), 2);
        assert_eq!(m.dated[&a][0].e, 3.0);
    }
}
