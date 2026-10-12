//! Phase 3 (INFRA.md §4.3): the one nonlinearity. An entry's resolved
//! embedded measure is read back per anchor (`m_a = sgnpow(e_a, w_a)`)
//! and pooled across anchors by nested signed `l_p` over the
//! single-linkage tree of the exponent matrix `K`.
//!
//! `K` is first replaced by its ultrametric closure over every anchor it
//! mentions, so the hierarchy is fixed rather than built from whichever
//! anchors one entry holds. Pairs are then merged in ascending `K`; two
//! clusters joined at level `l` pool as
//! `sgnpow(sgnpow(v_A, 1/l) + sgnpow(v_B, 1/l), l)`, and clusters still
//! apart at level 1 add. On a hierarchical (ultrametric) `K` this
//! is exactly the nested norm over that hierarchy, so the legacy
//! box/group pooling is the special case it was built from. The value is
//! monotone and continuous in `K`, degree-1 homogeneous in mass, never
//! exceeds the total mass, and adds across anchors with `K = 1`.
//!
//! Merging along a tree is the most associativity a per-pair exponent
//! allows: with three anchors at pairwise exponents `p, q, q`, merge
//! order is irrelevant only when `p = q`.
//!
//! The tree's leaves are boxes of twin anchors (`K(a, b) = w_a = w_b`),
//! which pool as one anchor. Within a box, dated mass first goes through
//! the time stage (`TimeParams`): a mix of pooling everything as one and
//! the concave coverage integral of its dated density. A tree can't take
//! its time structure from a time-difference kernel (on a continuous time
//! line its closure collapses to one level), which is why time is
//! integrated inside the leaves rather than through `K`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::measure::{sgnpow, Measure, Span};
use crate::types::{AnchorId, Contribution, KFeatMatrix, TimeParams};

/// Smallest exponent honoured; `w -> 0` would pool as an exact max and
/// overflow the embedding.
const MIN_EXPONENT: f64 = 1e-3;

fn clamp_exponent(w: f32) -> f64 {
    f64::from(w).clamp(MIN_EXPONENT, 1.0)
}

/// `K` densified over the anchors it mentions, so the per-entry hot loop
/// never clones an `AnchorId` for a lookup. Anchors it doesn't mention
/// default to exponent `1` and independence.
pub(crate) struct Kernel {
    index: HashMap<AnchorId, usize>,
    exponent: Vec<f64>,
    level: Vec<Vec<f64>>,
}

impl Kernel {
    pub(crate) fn new(k: &KFeatMatrix) -> Self {
        let anchors: BTreeSet<&AnchorId> = k.pairs.keys().flat_map(|(a, b)| [a, b]).collect();
        let index: HashMap<AnchorId, usize> = anchors
            .iter()
            .enumerate()
            .map(|(i, a)| ((*a).clone(), i))
            .collect();
        let n = index.len();
        let mut exponent = vec![1.0; n];
        let mut level = vec![vec![1.0; n]; n];
        for ((a, b), &v) in &k.pairs {
            let (i, j) = (index[a], index[b]);
            if i == j {
                exponent[i] = clamp_exponent(v);
            } else {
                level[i][j] = clamp_exponent(v);
                level[j][i] = level[i][j];
            }
        }
        // Pool over the closure of the whole matrix, not over whichever
        // anchors one entry happens to hold: chaining only through present
        // anchors would let a vanishing mass on a bridging anchor tighten
        // two others' pooling, so adding mass could lower the value.
        for m in 0..n {
            for i in 0..n {
                for j in 0..n {
                    let through = level[i][m].max(level[m][j]);
                    if i != j && i != m && j != m && through < level[i][j] {
                        level[i][j] = through;
                    }
                }
            }
        }
        Kernel {
            index,
            exponent,
            level,
        }
    }

    pub(crate) fn exponent_of(&self, a: &AnchorId) -> f64 {
        self.index.get(a).map_or(1.0, |&i| self.exponent[i])
    }

    fn level_of(&self, a: &AnchorId, b: &AnchorId) -> f64 {
        match (self.index.get(a), self.index.get(b)) {
            (Some(&i), Some(&j)) => self.level[i][j],
            _ => 1.0,
        }
    }

    /// Whether `a` and `b` are one box: same exponent, pooling at it.
    fn twins(&self, a: &AnchorId, b: &AnchorId) -> bool {
        match (self.index.get(a), self.index.get(b)) {
            (Some(&i), Some(&j)) => {
                self.exponent[i] == self.exponent[j] && self.level[i][j] <= self.exponent[i]
            }
            _ => false,
        }
    }
}

/// `∫ sgnpow(u(t), w) dt` for `u = c (box_L ⋆ density)`, `c = L^(-1/w)`,
/// where each span spreads its embedded mass evenly over its days. `u`
/// is piecewise linear with breaks at span ends `± L/2`, and
/// `F(u) = |u|^(w+1) / (w+1)` is an antiderivative of `sgnpow(u, w)` on
/// both signs, so each linear piece integrates exactly.
fn coverage(spans: &[Span], w: f64, length: f64) -> f64 {
    let half = length / 2.0;
    let c = length.powf(-1.0 / w);
    let u = |t: f64| -> f64 {
        c * spans
            .iter()
            .map(|s| {
                let overlap = (s.to.min(t + half) - s.from.max(t - half)).max(0.0);
                s.e / (s.to - s.from) * overlap
            })
            .sum::<f64>()
    };
    let mut breaks: Vec<f64> = spans
        .iter()
        .flat_map(|s| [s.from - half, s.from + half, s.to - half, s.to + half])
        .collect();
    breaks.sort_by(f64::total_cmp);
    breaks.dedup();
    let antiderivative = |u: f64| u.abs().powf(w + 1.0) / (w + 1.0);
    let mut total = 0.0;
    let mut prev = (breaks[0], u(breaks[0]));
    for &t in &breaks[1..] {
        let (t0, u0) = prev;
        let u1 = u(t);
        let h = t - t0;
        let mid = 0.5 * (u0 + u1);
        let r = (u1 - u0) / mid;
        total += if mid != 0.0 && r.abs() < 1e-3 {
            // Mean of sgnpow over a nearly flat piece, by Taylor expansion
            // about the midpoint (error O(r^4)); the difference quotient
            // would lose digits to cancellation here.
            h * sgnpow(mid, w) * (1.0 + w * (w - 1.0) * r * r / 24.0)
        } else if u0 == u1 {
            h * sgnpow(u0, w)
        } else {
            h * (antiderivative(u1) - antiderivative(u0)) / (u1 - u0)
        };
        prev = (t, u1);
    }
    total
}

/// One box's value: its undated embedded mass `undated` plus its dated
/// spans, through the time stage.
fn box_value(undated: f64, spans: &[Span], w: f64, time: &TimeParams) -> f64 {
    let dated: f64 = spans.iter().map(|s| s.e).sum();
    let full = sgnpow(undated + dated, w);
    let shared = f64::from(time.shared).clamp(0.0, 1.0);
    let length = f64::from(time.length_days);
    if spans.is_empty() || shared >= 1.0 || !(length > 0.0 && length.is_finite()) {
        return full;
    }
    let local = sgnpow(undated + sgnpow(coverage(spans, w, length), 1.0 / w), w);
    shared * full + (1.0 - shared) * local
}

/// One merge in the single-linkage tree.
struct Merge {
    level: f64,
    children: [usize; 2],
}

/// One leaf of the tree: a box of twin anchors present in the entry.
struct Leaf<'a> {
    anchors: Vec<(&'a AnchorId, f64)>,
    representative: &'a AnchorId,
    value: f64,
}

/// The pooled value of one entry, with each anchor's share (Euler shares
/// of the boxes, split within a box by embedded mass).
pub(crate) fn value(
    measure: &Measure,
    kernel: &Kernel,
    time: &TimeParams,
) -> (f32, Vec<Contribution>) {
    let mut present: BTreeMap<&AnchorId, (f64, Vec<Span>)> = BTreeMap::new();
    for (a, &e) in &measure.undated {
        if e != 0.0 {
            present.entry(a).or_default().0 += e;
        }
    }
    for (a, spans) in &measure.dated {
        let spans: Vec<Span> = spans.iter().filter(|s| s.e != 0.0).copied().collect();
        if !spans.is_empty() {
            present.entry(a).or_default().1.extend(spans);
        }
    }
    let mut leaves: Vec<Leaf> = Vec::new();
    let mut boxes: Vec<(Vec<&AnchorId>, f64, Vec<Span>)> = Vec::new();
    for (a, (undated, spans)) in present.iter() {
        match boxes.iter_mut().find(|b| kernel.twins(b.0[0], a)) {
            Some(b) => {
                b.0.push(a);
                b.1 += undated;
                b.2.extend(spans.iter().copied());
            }
            None => boxes.push((vec![*a], *undated, spans.clone())),
        }
    }
    for (anchors, undated, spans) in boxes {
        let w = kernel.exponent_of(anchors[0]);
        leaves.push(Leaf {
            anchors: anchors
                .iter()
                .map(|a| {
                    let (u, s) = &present[a];
                    (*a, u + s.iter().map(|s| s.e).sum::<f64>())
                })
                .collect(),
            representative: anchors[0],
            value: box_value(undated, &spans, w, time),
        });
    }
    let n = leaves.len();

    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            let l = kernel.level_of(leaves[i].representative, leaves[j].representative);
            if l < 1.0 {
                pairs.push((l, i, j));
            }
        }
    }
    pairs.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));

    // Tree nodes: leaves 0..n, then one node per merge. `root[i]` is the
    // current tree node holding union-find set `i`.
    let mut val: Vec<f64> = leaves.iter().map(|l| l.value).collect();
    let mut merges: Vec<Merge> = Vec::new();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut root: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (l, i, j) in pairs {
        let (a, b) = (find(&mut parent, i), find(&mut parent, j));
        if a == b {
            continue;
        }
        let (na, nb) = (root[a], root[b]);
        let p = 1.0 / l;
        val.push(sgnpow(sgnpow(val[na], p) + sgnpow(val[nb], p), l));
        merges.push(Merge {
            level: l,
            children: [na, nb],
        });
        parent[b] = a;
        root[a] = val.len() - 1;
    }
    let tops: BTreeSet<usize> = (0..n).map(|i| root[find(&mut parent, i)]).collect();
    let total: f64 = tops.iter().map(|&t| val[t]).sum();

    // dV/dv for every tree node, top-down: a child of a node at level `l`
    // carries `(|v_c| / |v_n|)^(1/l - 1)`; tops carry 1.
    let mut grad = vec![0.0f64; val.len()];
    for &t in &tops {
        grad[t] = 1.0;
    }
    for (k, merge) in merges.iter().enumerate().rev() {
        let node = n + k;
        let vn = val[node].abs();
        for &c in &merge.children {
            let vc = val[c].abs();
            grad[c] = if vn > 0.0 && vc > 0.0 {
                grad[node] * (vc / vn).powf(1.0 / merge.level - 1.0)
            } else {
                0.0
            };
        }
    }

    let mut contributions = Vec::new();
    for (i, leaf) in leaves.iter().enumerate() {
        let share = leaf.value * grad[i];
        let embedded: f64 = leaf.anchors.iter().map(|(_, e)| e).sum();
        for &(anchor, e) in &leaf.anchors {
            contributions.push(Contribution {
                anchor: anchor.clone(),
                embedded: e as f32,
                mass: sgnpow(e, kernel.exponent_of(anchor)) as f32,
                contribution: if embedded != 0.0 {
                    (share * e / embedded) as f32
                } else {
                    0.0
                },
            });
        }
    }
    contributions.sort_by(|a, b| a.anchor.cmp(&b.anchor));
    (total as f32, contributions)
}

/// One entry of `K` that its ultrametric closure tightens: scoring
/// already treats `a` and `b` as pooling at `to`, forced by the chain
/// `via` (from `a` to `b`, each hop tighter than or equal to `to`).
#[derive(Debug, Clone, PartialEq)]
pub struct KCorrection {
    pub a: AnchorId,
    pub b: AnchorId,
    pub from: f32,
    pub to: f32,
    pub via: Vec<AnchorId>,
}

/// The largest ultrametric below `K` over `anchors` (min over paths of
/// the loosest hop), as the entries it changes. Scoring is unchanged by
/// applying them: single linkage only ever reads the closure.
pub fn ultrametric_closure(k: &KFeatMatrix, anchors: &[AnchorId]) -> Vec<KCorrection> {
    let n = anchors.len();
    let mut u: Vec<Vec<f32>> = (0..n)
        .map(|i| (0..n).map(|j| k.get(&anchors[i], &anchors[j])).collect())
        .collect();
    let original = u.clone();
    let mut via: Vec<Vec<Option<usize>>> = vec![vec![None; n]; n];
    for m in 0..n {
        for i in 0..n {
            for j in 0..n {
                let through = u[i][m].max(u[m][j]);
                if i != j && i != m && j != m && through < u[i][j] {
                    u[i][j] = through;
                    via[i][j] = Some(m);
                }
            }
        }
    }
    fn chain(via: &[Vec<Option<usize>>], i: usize, j: usize, out: &mut Vec<usize>) {
        match via[i][j] {
            None => out.push(j),
            Some(m) => {
                chain(via, i, m, out);
                chain(via, m, j, out);
            }
        }
    }
    let mut out = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            if u[i][j] < original[i][j] {
                let mut path = vec![i];
                chain(&via, i, j, &mut path);
                out.push(KCorrection {
                    a: anchors[i].clone(),
                    b: anchors[j].clone(),
                    from: original[i][j],
                    to: u[i][j],
                    via: path.into_iter().map(|p| anchors[p].clone()).collect(),
                });
            }
        }
    }
    out
}

/// Writes `corrections` into `k`, giving its ultrametric closure.
pub fn apply_corrections(k: &mut KFeatMatrix, corrections: &[KCorrection]) {
    let mut set: BTreeMap<(AnchorId, AnchorId), f32> = BTreeMap::new();
    for c in corrections {
        k.pairs.remove(&(c.b.clone(), c.a.clone()));
        set.insert((c.a.clone(), c.b.clone()), c.to);
    }
    k.pairs.extend(set);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> AnchorId {
        AnchorId(s.into())
    }

    fn kernel(pairs: &[(&str, &str, f32)]) -> Kernel {
        let mut k = KFeatMatrix::default();
        for &(a, b, v) in pairs {
            k.pairs.insert((id(a), id(b)), v);
        }
        Kernel::new(&k)
    }

    fn measure(entries: &[(&str, f64)]) -> Measure {
        Measure {
            undated: entries.iter().map(|&(a, e)| (id(a), e)).collect(),
            ..Measure::default()
        }
    }

    fn value(m: &Measure, k: &Kernel) -> (f32, Vec<Contribution>) {
        super::value(m, k, &TimeParams::default())
    }

    #[test]
    fn independent_anchors_add() {
        let k = kernel(&[("a", "a", 0.5), ("b", "b", 0.3)]);
        let (v, c) = value(&measure(&[("a", 4.0), ("b", -8.0)]), &k);
        // m_a = 4^0.5 = 2, m_b = -(8^0.3)
        let expect = 2.0 - 8f64.powf(0.3);
        assert!((f64::from(v) - expect).abs() < 1e-5);
        let sum: f32 = c.iter().map(|c| c.contribution).sum();
        assert!((sum - v).abs() < 1e-5);
    }

    #[test]
    fn twins_pool_as_one_anchor() {
        let w = 0.4;
        let k = kernel(&[("a", "a", w), ("b", "b", w), ("a", "b", w)]);
        let (split, _) = value(&measure(&[("a", 3.0), ("b", 5.0)]), &k);
        let (whole, _) = value(&measure(&[("a", 8.0)]), &k);
        assert!((split - whole).abs() < 1e-5);
    }

    #[test]
    fn nested_levels_match_the_nested_norm() {
        let k = kernel(&[
            ("a", "a", 0.3),
            ("b", "b", 0.3),
            ("c", "c", 0.5),
            ("a", "b", 0.3),
            ("a", "c", 0.6),
            ("b", "c", 0.6),
        ]);
        let (ea, eb, ec) = (2.0f64, 3.0f64, 1.5f64);
        let (v, c) = value(&measure(&[("a", ea), ("b", eb), ("c", ec)]), &k);
        let box_ab = (ea + eb).powf(0.3);
        let mc = ec.powf(0.5);
        let expect = (box_ab.powf(1.0 / 0.6) + mc.powf(1.0 / 0.6)).powf(0.6);
        assert!((f64::from(v) - expect).abs() < 1e-5);
        let sum: f32 = c.iter().map(|c| c.contribution).sum();
        assert!((sum - v).abs() < 1e-4);
    }

    fn span(from: f64, to: f64, e: f64) -> Span {
        Span { from, to, e }
    }

    #[test]
    fn coverage_depends_only_on_the_density() {
        let w = 0.4;
        let whole = coverage(&[span(0.0, 100.0, 5.0)], w, 30.0);
        let split = coverage(&[span(0.0, 40.0, 2.0), span(40.0, 100.0, 3.0)], w, 30.0);
        assert!((whole - split).abs() < 1e-9 * whole, "{whole} vs {split}");
    }

    #[test]
    fn a_short_span_scores_about_its_own_mass() {
        let (w, e, d, l) = (0.35, 8.0f64, 1.0, 365.0);
        let exact = e.powf(w) * (1.0 - d / l + 2.0 * d / ((w + 1.0) * l));
        assert!((coverage(&[span(0.0, d, e)], w, l) - exact).abs() < 1e-9);
    }

    #[test]
    fn distant_repeats_count_partly_new() {
        let (w, e) = (0.4, 2.0f64);
        let spans = [span(0.0, 1.0, e), span(10_000.0, 10_001.0, e)];
        let lone = coverage(&spans[..1], w, 30.0);
        let time = |shared| TimeParams {
            shared,
            length_days: 30.0,
        };
        let full = (2.0 * e).powf(w);
        assert!((box_value(0.0, &spans, w, &time(1.0)) - full).abs() < 1e-12);
        let mixed = box_value(0.0, &spans, w, &time(0.7));
        let shared = f64::from(0.7f32);
        let expect = shared * full + (1.0 - shared) * 2.0 * lone;
        assert!((mixed - expect).abs() < 1e-9, "{mixed} vs {expect}");
    }

    #[test]
    fn undated_mass_alone_ignores_time() {
        let time = TimeParams::default();
        assert_eq!(box_value(3.0, &[], 0.4, &time), 3f64.powf(0.4));
    }

    #[test]
    fn a_vanishing_bridge_anchor_changes_nothing() {
        // visual -0.7- hype -0.7- comedy; visual–comedy unlisted (1).
        let k = kernel(&[
            ("visual", "visual", 0.4),
            ("hype", "hype", 0.4),
            ("comedy", "comedy", 0.4),
            ("visual", "hype", 0.7),
            ("hype", "comedy", 0.7),
        ]);
        let (without, _) = value(&measure(&[("visual", 2.0), ("comedy", 2.0)]), &k);
        let (with, _) = value(
            &measure(&[("visual", 2.0), ("hype", 1e-12), ("comedy", 2.0)]),
            &k,
        );
        assert!((with - without).abs() < 1e-5, "{with} vs {without}");
    }

    #[test]
    fn closure_reports_the_forcing_chain() {
        let mut k = KFeatMatrix::default();
        for (a, b, v) in [("hype", "comedy", 0.4), ("visual", "hype", 0.7)] {
            k.pairs.insert((id(a), id(b)), v);
        }
        let anchors = [id("comedy"), id("hype"), id("visual")];
        let fixes = ultrametric_closure(&k, &anchors);
        assert_eq!(fixes.len(), 1);
        assert_eq!((fixes[0].a.0.as_str(), fixes[0].b.0.as_str()), ("comedy", "visual"));
        assert_eq!(fixes[0].to, 0.7);
        assert_eq!(fixes[0].via, vec![id("comedy"), id("hype"), id("visual")]);
        let before = Kernel::new(&k);
        apply_corrections(&mut k, &fixes);
        assert!(ultrametric_closure(&k, &anchors).is_empty());
        let m = measure(&[("comedy", 2.0), ("hype", 1.0), ("visual", 3.0)]);
        assert_eq!(value(&m, &before).0, value(&m, &Kernel::new(&k)).0);
    }
}
