//! The frozen contract (INFRA.md §4.2). Every other crate negotiates
//! through these types; nothing here touches I/O.
//!
//! Sparse maps keyed by anchor/entry ids use `BTreeMap` rather than the
//! `HashMap` the doc sketches with, so that iteration order (and thus
//! floating-point summation order in `score`) is deterministic given
//! identical input — required for the engine to be property-testable
//! (INFRA.md §4.1).

use std::collections::BTreeMap;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        pub struct $name(pub String);
    };
}

id_type!(EntryId);
id_type!(ImpactId);
id_type!(SourceId);
id_type!(AnchorId);

/// The whole scoring substrate at a point in time (INFRA.md §4.2, §5.1).
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    pub impacts: Vec<Impact>,
    pub relations: Vec<Relation>,
    pub palette: Palette,
}

/// Opaque id plus a pointer into a type store; the central DB never
/// stores a provider id itself (INFRA.md §5.1, §5.2).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Entry {
    pub id: EntryId,
    pub store: String,
    pub store_id: String,
    pub title_cache: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Impact {
    pub id: ImpactId,
    pub target: EntryId,
    pub atoms: Vec<Atom>,
}

/// One value mass: a source, a blend over 1-3 palette anchors, a signed
/// magnitude, and when it happened.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Atom {
    pub source: SourceId,
    pub blend: Vec<(AnchorId, f32)>,
    pub magnitude: f32,
    pub time: TimeSupport,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TimeSupport {
    /// Undated: the magnitude is the whole mass.
    Moment,
    /// Undated, with a relative decay profile whose area scales the
    /// magnitude.
    Tail { peak: f32, lambda: f32, cutoff: f32 },
    /// Dated: the magnitude is the value of the whole period pooled as
    /// one (the legacy duration formulas), and its embedded mass is
    /// spread evenly over days `[from, to)`, counted from 1970-01-01. A
    /// period shorter than a day counts as one day.
    Interval { from: i32, to: i32 },
}

/// Relation kinds (INFRA.md §5.1 `relation.kind`). Not read by the
/// scoring math (`measure`/`solve`/`pool` only look at
/// `from`/`to`/`transform`); a semantic label for downstream consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RelKind {
    /// Genuine structural containment: a franchise/album nested around
    /// its works/tracks (nested XML entries or an explicit `<contains>`
    /// tag).
    Contains,
    /// A contributor (artist, writer, ...) borrowing a share of a work
    /// they're credited on. Same borrow mechanics as `Contains`, but not
    /// structural containment.
    Credit,
    Remix,
    FeatureMusic,
    KilledBy,
}

/// A scalar field `s_r(x)` over the anchor space (notes §8.3).
/// `Transport` is rare and discouraged (INFRA.md §4.2).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Transform {
    Scalar(f32),
    Regional(BTreeMap<AnchorId, f32>),
    // JSON object keys must be strings, so a `BTreeMap<(AnchorId, AnchorId),
    // f32>` can't derive directly (serde_json errors at the first non-empty
    // map). Represented on the wire as a list of `(a, b, k)` triples instead.
    #[cfg_attr(feature = "serde", serde(with = "transport_pairs"))]
    Transport(BTreeMap<(AnchorId, AnchorId), f32>),
}

#[cfg(feature = "serde")]
mod transport_pairs {
    use super::AnchorId;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    pub fn serialize<S>(
        map: &BTreeMap<(AnchorId, AnchorId), f32>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let triples: Vec<(&AnchorId, &AnchorId, f32)> =
            map.iter().map(|((a, b), k)| (a, b, *k)).collect();
        triples.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<(AnchorId, AnchorId), f32>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let triples = Vec::<(AnchorId, AnchorId, f32)>::deserialize(deserializer)?;
        Ok(triples.into_iter().map(|(a, b, k)| ((a, b), k)).collect())
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Relation {
    pub kind: RelKind,
    pub from: EntryId,
    pub to: EntryId,
    pub transform: Transform,
}

/// A named palette anchor (INFRA.md §5.1 `anchor` table). `family` is a
/// soft label only (emotion|craft|...), not load-bearing for scoring.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Anchor {
    pub id: AnchorId,
    pub label: String,
    pub family: Option<String>,
}

/// Sparse anchor x anchor pooling-exponent matrix `K` (INFRA.md §5.1
/// `kfeat`: "sparse: only non-default pairs"). Unlisted entries default
/// to `1.0`.
///
/// - The diagonal `K(a, a) = w_a` is anchor `a`'s own exponent: repeated
///   value on `a` pools as `l_(1/w_a)` (embedded mass `e = m^(1/w_a)` adds),
///   so `n` equal hits grow like `n^w_a`. `1.0` means plain addition.
/// - Off the diagonal, `K(a, b)` is the exponent at which `a` and `b` pool
///   together: `1.0` is independent (they add), and `K(a, b) = w_a = w_b`
///   makes them one box (twins). Meaningful values satisfy
///   `max(w_a, w_b) <= K(a, b) <= 1`.
///
/// Scoring reads `K` through its single-linkage tree, so only its
/// ultrametric closure matters (see [`crate::ultrametric_closure`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KFeatMatrix {
    pub pairs: BTreeMap<(AnchorId, AnchorId), f32>,
}

impl KFeatMatrix {
    pub fn get(&self, a: &AnchorId, b: &AnchorId) -> f32 {
        // The lookup key clones both `AnchorId` `String`s (a `BTreeMap<(K,
        // K), V>` has no borrowed-key query path for a tuple), so skip it
        // entirely when there's nothing to find.
        if self.pairs.is_empty() {
            return 1.0;
        }
        self.pairs
            .get(&(a.clone(), b.clone()))
            .or_else(|| self.pairs.get(&(b.clone(), a.clone())))
            .copied()
            .unwrap_or(1.0)
    }

    /// Anchor `a`'s own exponent `w_a = K(a, a)`.
    pub fn exponent(&self, a: &AnchorId) -> f32 {
        self.get(a, a)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub anchors: Vec<Anchor>,
    pub k_feat: KFeatMatrix,
}

/// Monotone calibration `g`: piecewise-linear through sorted `(v, s)`
/// knots (INFRA.md §5.1 `calibration.isotonic`).
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Isotonic {
    pub knots: Vec<(f32, f32)>,
}

impl Isotonic {
    /// `g(v)`: linear interpolation between the knots bracketing `v`,
    /// clamped to the end knots' values outside their range. The
    /// identity map if no knots are set.
    pub fn apply(&self, v: f32) -> f32 {
        let Some(&(first_x, first_y)) = self.knots.first() else {
            return v;
        };
        if v <= first_x {
            return first_y;
        }
        let &(last_x, last_y) = self.knots.last().unwrap();
        if v >= last_x {
            return last_y;
        }
        for pair in self.knots.windows(2) {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            if v >= x0 && v <= x1 {
                let t = if x1 > x0 { (v - x0) / (x1 - x0) } else { 0.0 };
                return y0 + t * (y1 - y0);
            }
        }
        v
    }
}

/// How dated mass on one box of twin anchors decorrelates over time.
///
/// The box's value is `shared * full + (1 - shared) * local`, where
/// `full` pools all of its mass as one regardless of timing (the legacy
/// behaviour) and `local` is the concave coverage integral
/// `∫ sgnpow((phi ⋆ e)(t), w) dt` of its dated embedded density, with
/// `phi` a box of width `length_days` normalised so a lone instant
/// scores its own mass. Within `length_days` repeats pool as one; across
/// longer gaps a share `1 - shared` of each counts as new. Undated mass
/// pools with the local part as if it happened at the same moment.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TimeParams {
    /// `λ` in `[0, 1]`; 1 ignores time entirely.
    pub shared: f32,
    /// `L` in days, `> 0`.
    pub length_days: f32,
}

impl Default for TimeParams {
    fn default() -> Self {
        TimeParams {
            shared: 0.7,
            length_days: 365.0,
        }
    }
}

/// How positive and negative mass on one entry combine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum SignMode {
    /// Positive and negative atoms pool as two separate measures and the
    /// value is `V(mu+) - V(mu-)`: a negative is an independent bad
    /// experience and costs its standalone value, however much positive
    /// mass sits beside it.
    #[default]
    Split,
    /// One signed measure: opposite masses cancel in embedded units, so a
    /// negative costs only what a positive of its size would add there.
    Signed,
}

/// `Theta` - the fitted knobs (INFRA.md §4.2): the pooling-exponent
/// matrix, the time decorrelation, the public-scale calibration, and
/// how signs combine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Params {
    pub k_feat: KFeatMatrix,
    pub time: TimeParams,
    pub calibration: Isotonic,
    pub sign: SignMode,
}

/// `score`'s output: public scale `S_e` per entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scores {
    pub values: BTreeMap<EntryId, f32>,
}

/// One anchor's share of an entry's value. `embedded` is the resolved
/// signed embedded mass `e`, `mass` its value-unit form
/// `sgn(e) |e|^w`, and `contribution` its Euler share of the value
/// (`mass * dV/dmass`; the shares sum to the value). Under
/// `SignMode::Split` each field is the anchor's positive part minus its
/// negative part.
#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    pub anchor: AnchorId,
    pub embedded: f32,
    pub mass: f32,
    pub contribution: f32,
}

/// One impact's part in an anchor's contribution. `entry` holds the
/// impact: the explained entry itself, or one it borrows from (through
/// `origin_anchor`, which differs from `anchor` only under `Transport`).
/// `embedded` is the impact's embedded mass that reaches `anchor`; the
/// sources of one anchor sum to its `embedded`. `contribution` is the
/// anchor's contribution split by that embedded mass, so the sources of
/// one anchor sum to its contribution as well.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub anchor: AnchorId,
    pub entry: EntryId,
    pub origin_anchor: AnchorId,
    pub impact: ImpactId,
    pub embedded: f32,
    pub contribution: f32,
}

/// `explain`'s output: the per-cell breakdown behind one entry's score.
#[derive(Debug, Clone, PartialEq)]
pub struct Explanation {
    pub entry: EntryId,
    pub value: f32,
    pub score: f32,
    pub contributions: Vec<Contribution>,
    pub sources: Vec<Source>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kfeat_defaults_to_one_everywhere() {
        let m = KFeatMatrix::default();
        let a = AnchorId("gut-punch".into());
        let b = AnchorId("gorgeous-visuals".into());
        assert_eq!(m.exponent(&a), 1.0);
        assert_eq!(m.get(&a, &b), 1.0);
    }

    #[test]
    fn kfeat_explicit_pair_is_symmetric() {
        let a = AnchorId("gut-punch".into());
        let b = AnchorId("gorgeous-visuals".into());
        let mut m = KFeatMatrix::default();
        m.pairs.insert((a.clone(), b.clone()), 0.4);
        assert_eq!(m.get(&a, &b), 0.4);
        assert_eq!(m.get(&b, &a), 0.4);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn transform_transport_round_trips_through_json() {
        let a = AnchorId("gut-punch".into());
        let b = AnchorId("gorgeous-visuals".into());
        let mut pairs = BTreeMap::new();
        pairs.insert((a.clone(), b.clone()), 0.4);
        let t = Transform::Transport(pairs);

        let json = serde_json::to_string(&t).unwrap();
        let back: Transform = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn transform_scalar_and_regional_round_trip_through_json() {
        let scalar = Transform::Scalar(0.2);
        let json = serde_json::to_string(&scalar).unwrap();
        assert_eq!(serde_json::from_str::<Transform>(&json).unwrap(), scalar);

        let mut regional = BTreeMap::new();
        regional.insert(AnchorId("gut-punch".into()), 0.5);
        let regional = Transform::Regional(regional);
        let json = serde_json::to_string(&regional).unwrap();
        assert_eq!(serde_json::from_str::<Transform>(&json).unwrap(), regional);
    }
}
