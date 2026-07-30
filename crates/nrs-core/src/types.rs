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
pub struct Entry {
    pub id: EntryId,
    pub store: String,
    pub store_id: String,
    pub title_cache: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Impact {
    pub id: ImpactId,
    pub target: EntryId,
    pub atoms: Vec<Atom>,
}

/// One embedded mass: a source, a blend over 1-3 palette anchors, a
/// signed magnitude, and how it's spread over time.
#[derive(Debug, Clone, PartialEq)]
pub struct Atom {
    pub source: SourceId,
    pub blend: Vec<(AnchorId, f32)>,
    pub magnitude: f32,
    pub time: TimeSupport,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimeSupport {
    Moment,
    Tail { peak: f32, lambda: f32, cutoff: f32 },
}

/// Relation kinds (INFRA.md §5.1 `relation.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelKind {
    Contains,
    Remix,
    FeatureMusic,
    KilledBy,
}

/// A scalar field `s_r(x)` over the anchor space (notes §8.3).
/// `Transport` is rare and discouraged (INFRA.md §4.2).
#[derive(Debug, Clone, PartialEq)]
pub enum Transform {
    Scalar(f32),
    Regional(BTreeMap<AnchorId, f32>),
    Transport(BTreeMap<(AnchorId, AnchorId), f32>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Relation {
    pub kind: RelKind,
    pub from: EntryId,
    pub to: EntryId,
    pub transform: Transform,
}

/// A named palette anchor (INFRA.md §5.1 `anchor` table). `family` is a
/// soft label only (emotion|craft|...), not load-bearing for scoring.
#[derive(Debug, Clone, PartialEq)]
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

/// Deepest-shared-ancestor taxonomy, derived from `Contains` relations
/// rather than stored separately (INFRA.md §5.1): `parent` maps an
/// entry to its immediate container, if any.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Taxonomy {
    pub parent: BTreeMap<EntryId, EntryId>,
}

/// The ~5 taxonomy-distance levels feeding `k_src` (INFRA.md §4.2).
/// Exact semantics are pinned down when `nrs-fit` lands (roadmap phase 6);
/// frozen here only as a fixed-size fitted vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KSrcLevels {
    pub levels: [f32; 5],
}

/// Degree-1 vs degree-q homogeneity of the redundancy functional
/// (notes §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Homogeneity {
    Degree1,
    DegreeQ,
}

/// Monotone calibration `g`: piecewise-linear through sorted `(v, s)`
/// knots (INFRA.md §5.1 `calibration.isotonic`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Isotonic {
    pub knots: Vec<(f32, f32)>,
}

/// `Theta` - the fitted knobs (INFRA.md §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub q: f32,
    pub sigma: f32,
    pub k_src_levels: KSrcLevels,
    pub k_feat: KFeatMatrix,
    pub homogeneity: Homogeneity,
    pub calibration: Isotonic,
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
}
