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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::measure::{sgnpow, Measure};
use crate::types::{AnchorId, Contribution, KFeatMatrix};

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
}

/// One merge in the single-linkage tree.
struct Merge {
    level: f64,
    children: [usize; 2],
}

/// The pooled value of one entry, with each anchor's Euler share.
pub(crate) fn value(measure: &Measure, kernel: &Kernel) -> (f32, Vec<Contribution>) {
    let leaves: Vec<(&AnchorId, f64, f64)> = measure
        .iter()
        .filter(|(_, &e)| e != 0.0)
        .map(|(a, &e)| (a, e, sgnpow(e, kernel.exponent_of(a))))
        .collect();
    let n = leaves.len();

    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            let l = kernel.level_of(leaves[i].0, leaves[j].0);
            if l < 1.0 {
                pairs.push((l, i, j));
            }
        }
    }
    pairs.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));

    // Tree nodes: leaves 0..n, then one node per merge. `root[i]` is the
    // current tree node holding union-find set `i`.
    let mut val: Vec<f64> = leaves.iter().map(|l| l.2).collect();
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

    let contributions = leaves
        .iter()
        .enumerate()
        .map(|(i, &(anchor, e, m))| Contribution {
            anchor: anchor.clone(),
            embedded: e as f32,
            mass: m as f32,
            contribution: (m * grad[i]) as f32,
        })
        .collect();
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
        entries.iter().map(|&(a, e)| (id(a), e)).collect()
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
