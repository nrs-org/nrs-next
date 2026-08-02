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
    pub taxonomy: Taxonomy,
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

/// One embedded mass: a source, a blend over 1-3 palette anchors, a
/// signed magnitude, and how it's spread over time.
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
    Moment,
    Tail { peak: f32, lambda: f32, cutoff: f32 },
}

/// Relation kinds (INFRA.md §5.1 `relation.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RelKind {
    Contains,
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

/// Sparse anchor x anchor redundancy matrix (INFRA.md §5.1 `kfeat`:
/// "sparse: only non-default pairs"). Unlisted pairs default to `1.0`
/// on the diagonal and `0.0` off it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KFeatMatrix {
    pub pairs: BTreeMap<(AnchorId, AnchorId), f32>,
}

impl KFeatMatrix {
    pub fn get(&self, a: &AnchorId, b: &AnchorId) -> f32 {
        if a == b {
            return 1.0;
        }
        // The lookup key clones both `AnchorId` `String`s (a `BTreeMap<(K,
        // K), V>` has no borrowed-key query path for a tuple), so skip it
        // entirely when there's nothing to find — the common case today,
        // since nothing populates `pairs` yet (INFRA.md's `nrs-fit` phase),
        // and this sits in `score()`'s O(cells^2) hot loop.
        if self.pairs.is_empty() {
            return 0.0;
        }
        self.pairs
            .get(&(a.clone(), b.clone()))
            .or_else(|| self.pairs.get(&(b.clone(), a.clone())))
            .copied()
            .unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub anchors: Vec<Anchor>,
    pub k_feat: KFeatMatrix,
}

/// Deepest-shared-ancestor taxonomy feeding `k_src` (INFRA.md §5.1),
/// derived from `Contains`/franchise relations rather than stored
/// separately: `parent` maps a source to its immediate container, if
/// any. Keyed by `SourceId` (not `EntryId`) because `k_src` compares
/// sources (characters/scenes/works), which needn't all be top-level
/// scoreable entries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Taxonomy {
    pub parent: BTreeMap<SourceId, SourceId>,
}

/// The ~5 taxonomy-distance levels feeding `k_src` (INFRA.md §4.2).
/// Exact semantics are pinned down when `nrs-fit` lands (roadmap phase 6);
/// frozen here only as a fixed-size fitted vector.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KSrcLevels {
    pub levels: [f32; 5],
}

/// Which redundancy functional to score with. The first two are the
/// per-cell additive forms (notes §7.2, degree-1 vs degree-q
/// homogeneity); `Spectral` is the bounded-slope spectral value
/// (redesign-tex `spectral-bounded-slope.tex`), a structurally
/// different construction that reads the spectrum of the mass-weighted
/// kernel rather than a per-cell crowding sum.
///
/// `Eq` is intentionally not derived: `Spectral` carries an `f32`
/// tuning constant, and `Params` (its only embedder) needs only
/// `PartialEq`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Homogeneity {
    Degree1,
    DegreeQ,
    /// Score via the eigenvalues `sigma_k` of the mass-weighted kernel
    /// `G = M^{1/2} K M^{1/2}` with the concave, bounded-origin-slope
    /// gauge `g(t) = beta * ln(1 + t/beta)`: `V = sum_k g(sigma_k)`.
    ///
    /// Unlike `DegreeQ` this is unconditionally monotone and exactly
    /// splitting-invariant; unlike the Schatten spectral form it stays
    /// finite under fixed-spread refinement, because `g` has finite
    /// slope at the origin. It gives up exact degree-1 homogeneity
    /// (which `DegreeQ` also lacks) — provably the price of that
    /// convergence (see the note's impossibility theorem).
    ///
    /// `beta` is the saturation scale, in mass units: `beta -> inf`
    /// recovers the kernel-blind `sum |mass|`, smaller `beta` sharpens
    /// the redundancy discount. Overall output scale is left to the
    /// downstream `calibration`, so there is no separate `alpha`.
    Spectral {
        beta: f32,
    },
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

/// How a relation's raw transform weight (`Transform`'s scalar(s) —
/// e.g. `killedBy`'s ~0.04 dilution) relates to the redundancy
/// exponent `q`.
///
/// Under `Fixed`, a relation-borrowed atom's contribution once
/// isolated is `(w*m)^q`; as `q -> 0`, `x^q -> 1` for *any* fixed
/// `x > 0`, so the weight `w` washes out almost entirely no matter
/// how small it is — a heavily-diluted relation ends up contributing
/// close to full presence credit at low `q`. `PowQ` reparametrizes
/// the weight as `w^(1/q)` before the relation solve, so the isolated
/// contribution becomes `(m*w^(1/q))^q = m^q * w` — `w` comes out
/// exactly linearly, for every `q`, matching what the dilution factor
/// was semantically supposed to mean throughout the whole range, not
/// just at `q=1`. `Contains`'s weight (`1.0`) is unaffected at any
/// `q` under either mode (`1^x = 1`), so only genuinely-diluted
/// relations (remix, killedBy, featureMusic) are touched.
///
/// `Fixed` is the default and matches INFRA.md/the redesign notes'
/// original spec exactly (including Prop. "the double-count is
/// identified and vanishes with saturation"'s closed form). `PowQ` is
/// a deliberate amendment on top of that spec to fix the low-q
/// pathology above; redesign-tex's notes prove `PowQ`'s `V(C) <=`
/// `Fixed`'s `V(C)` for `q <= 1` (the saturation/breadth regime) — the
/// reparametrization doesn't just fix the pathology, it provably
/// tightens the same double-count bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RelationScaling {
    #[default]
    Fixed,
    PowQ,
}

/// `Theta` - the fitted knobs (INFRA.md §4.2). No kernel width here:
/// `k_feat` and `k_src_levels` are already-realized lookup tables, not
/// a distance formula, so there's nothing for a width to parameterize
/// at this layer. A width belongs, if ever needed, to whatever
/// offline process derives one of those tables from an anchor
/// embedding — scoped to that step, not threaded through `score`.
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub q: f32,
    pub k_src_levels: KSrcLevels,
    pub k_feat: KFeatMatrix,
    pub homogeneity: Homogeneity,
    pub calibration: Isotonic,
    pub relation_scaling: RelationScaling,
}

/// `score`'s output: public scale `S_e` per entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scores {
    pub values: BTreeMap<EntryId, f32>,
}

/// One `(source, anchor)` cell's contribution to an entry's value
/// (INFRA.md §4.3 phase 3: `V = sum f * phi(c)`).
#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    pub source: SourceId,
    pub anchor: AnchorId,
    pub mass: f32,
    pub crowding: f32,
    pub contribution: f32,
}

/// `explain`'s output: the per-cell breakdown behind one entry's score.
#[derive(Debug, Clone, PartialEq)]
pub struct Explanation {
    pub entry: EntryId,
    pub value: f32,
    pub score: f32,
    pub contributions: Vec<Contribution>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kfeat_defaults_to_identity_diagonal() {
        let m = KFeatMatrix::default();
        let a = AnchorId("gut-punch".into());
        let b = AnchorId("gorgeous-visuals".into());
        assert_eq!(m.get(&a, &a), 1.0);
        assert_eq!(m.get(&a, &b), 0.0);
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
