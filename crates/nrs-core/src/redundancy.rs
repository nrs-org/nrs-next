//! Phase 3 (INFRA.md §4.3, notes ch. "The value kernel, up close"):
//! the one nonlinearity. Per entry, given its fully-resolved measure
//! `f`, crowding is `c = K * f` with `K = k_src . k_feat` (`k_time`
//! fixed at 1, notes §"What we integrate over"), and the entry's value
//! is `V = sum f(x) . phi(c(x))`, `phi(c) = c^(q-1)` (Def. "the
//! redundancy exponent q").
//!
//! The notes' worked examples are all same-signed masses, so two things
//! about mixed signs are modelling calls, not given:
//!
//! - Crowding sums `|mass|` rather than signed mass, so `phi`'s
//!   fractional power is always applied to a nonnegative number
//!   (avoiding complex results), while the outer product
//!   `f(x) . phi(c(x))` keeps the cell's own sign.
//! - A cell only crowds *same-signed* cells. A successful gut-punch
//!   elsewhere and an unrelated failed gut-punch attempt are opposite
//!   outcomes, not "the same value counted twice" — so a negative
//!   atom must not discount a nearby positive one just for sharing an
//!   anchor, or vice versa. Equivalently: positive and negative mass
//!   live at mirrored, mutually non-redundant points, without actually
//!   doubling the anchor space. This also means a pile of small
//!   negatives saturates under `q` the same way a pile of small
//!   positives does, rather than subtracting without bound.

use std::collections::HashMap;

#[cfg(feature = "faer-eigen")]
use crate::linalg::faer_eigen;
use crate::linalg::{householder_ql_eigen, householder_ql_eigenvalues};
use crate::measure::Measure;
use crate::types::{
    AnchorId, Contribution, Homogeneity, KFeatMatrix, KSrcLevels, SourceId, Taxonomy,
};

/// Memoized `k_src` evaluator. `ancestor_chain` used to rebuild a fresh
/// `Vec<SourceId>` (cloning every `String` on the path to the root) on
/// *every* pairwise call — with a hot loop making O(cells^2) calls per
/// entry, that's millions of heap allocations for the real dataset's
/// biggest franchise entries. `SourceIndex` interns each `SourceId` to a
/// small integer exactly once (the first time it's seen, across the
/// *whole* `score()`/`explain()` call, not per entry — siblings under
/// the same franchise share sources) and caches its ancestor chain as a
/// plain `Vec<u32>`, so every later `k_src` lookup is pointer-chasing
/// over integers, not string comparisons or allocations.
#[derive(Default)]
pub(crate) struct SourceIndex {
    id_of: HashMap<SourceId, u32>,
    chains: Vec<Option<Vec<u32>>>,
}

impl SourceIndex {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn intern(&mut self, id: &SourceId) -> u32 {
        if let Some(&i) = self.id_of.get(id) {
            return i;
        }
        let i = self.chains.len() as u32;
        self.id_of.insert(id.clone(), i);
        self.chains.push(None);
        i
    }

    /// Interns `id` (if needed) and returns its ancestor chain, building
    /// and caching it on first request.
    fn ensure_chain(&mut self, taxonomy: &Taxonomy, id: &SourceId) -> usize {
        let i = self.intern(id) as usize;
        if self.chains[i].is_none() {
            let mut chain = vec![i as u32];
            let mut current = id.clone();
            while let Some(parent) = taxonomy.parent.get(&current) {
                let p = self.intern(parent);
                if chain.contains(&p) {
                    break;
                }
                chain.push(p);
                current = parent.clone();
            }
            self.chains[i] = Some(chain);
        }
        i
    }

    /// `k_src`: 1.0 at an identical source; otherwise the level indexed
    /// by the combined tree-distance to the deepest shared ancestor
    /// (notes table: same character/scene, same work, same franchise,
    /// same genre/vibe, unrelated), falling back to the "unrelated"
    /// level when the two sources share no ancestor at all. The exact
    /// distance-to-level mapping is a modelling choice pending `nrs-fit`
    /// (roadmap phase 6); this only needs to be monotone in "how far
    /// apart".
    ///
    /// Any combined distance `>= levels.len()` maps to the same last
    /// ("unrelated") level as no match at all (the `.min(levels.len() -
    /// 1)` clamp below), so the search below only ever needs to look
    /// that far into each chain — bounding what would otherwise be a
    /// full ancestor-chain scan (root-to-leaf, potentially much deeper
    /// than `levels.len()` for a nested taxonomy) to a fixed, small
    /// worst case. This matters once `spectral_class` stops skipping
    /// `k_src` for most cross-anchor pairs (a populated `k_feat`), the
    /// only production caller that's genuinely hot: `bucket_crowding`'s
    /// depth-histogram trick already avoids calling this at all.
    pub(crate) fn k_src(
        &mut self,
        a: &SourceId,
        b: &SourceId,
        taxonomy: &Taxonomy,
        levels: &KSrcLevels,
    ) -> f32 {
        if a == b {
            return 1.0;
        }
        let ia = self.ensure_chain(taxonomy, a);
        let ib = self.ensure_chain(taxonomy, b);
        let chain_a = self.chains[ia].as_ref().unwrap();
        let chain_b = self.chains[ib].as_ref().unwrap();
        let max_dist = levels.levels.len();
        for (da, &node) in chain_a.iter().take(max_dist).enumerate() {
            let searchable = &chain_b[..chain_b.len().min(max_dist - da)];
            if let Some(db) = searchable.iter().position(|&n| n == node) {
                let dist = da + db;
                let idx = dist.saturating_sub(1).min(levels.levels.len() - 1);
                return levels.levels[idx];
            }
        }
        *levels.levels.last().unwrap()
    }

    /// The interned ancestor chain of `id` (`chain[0] == id`'s own
    /// interned id, `chain[1]` its parent, ...), for callers that need
    /// to aggregate over many sources' ancestry at once (`bucket_crowding`)
    /// rather than compare one pair at a time.
    fn chain(&mut self, taxonomy: &Taxonomy, id: &SourceId) -> &[u32] {
        let i = self.ensure_chain(taxonomy, id);
        self.chains[i].as_ref().unwrap()
    }
}

/// `k_src_levels` always has this many entries (INFRA.md §4.2's "~5
/// levels"), so a tree-distance `>= MAX_K_SRC_DIST` always saturates to
/// the last ("unrelated") level regardless of exactly how far apart two
/// sources are — `bucket_crowding` only needs *exact* per-pair distances
/// up to here, and can lump everything past it into one aggregate.
const MAX_K_SRC_DIST: usize = 4;

/// Sum of `k_src(s_i, s_j) * mass_j` over every `j` (including `i`
/// itself, via the exact `k_src(i,i)=1` self term) for every `i`, over a
/// bucket of sources that are already known to share one `k_feat` weight
/// (typically 1.0, the same-anchor diagonal) — i.e. this computes the
/// per-cell crowding that a naive `O(bucket^2)` all-pairs comparison
/// would, but in `O(bucket * MAX_K_SRC_DIST)`.
///
/// The trick: `k_src(i,j)` only depends on `da + db`, the combined
/// tree-distance from `i` and `j` to their lowest common ancestor. So
/// rather than compare every pair, build `depth_hist[node][e]` = total
/// bucket mass sitting at exact depth `e` (`e <= MAX_K_SRC_DIST`) below
/// `node`, in one `O(bucket * MAX_K_SRC_DIST)` pass over every source's
/// own ancestor chain. Then for query source `i`, walking `i`'s own
/// chain `A_0=i, A_1, ...` and reading `depth_hist[A_da][db]` gives the
/// mass of *every* bucket source at combined distance `da+db` from `i`
/// whose ancestor chain passes through `A_da` at depth `db` — except
/// that also includes sources closer to `i` (whose true LCA with `i` is
/// some shallower `A_{da'}`, `da' < da`, not `A_da`), which is exactly
/// the mass `depth_hist[A_{da-1}][db-1]` already walked through: one
/// less hop from `i`, but the same node overall (subtracting it is the
/// "claimed by a shallower ancestor" correction below). Whatever mass
/// isn't accounted for by any `(da, db)` with `da+db <= MAX_K_SRC_DIST`
/// is, by construction, every source at combined distance `>
/// MAX_K_SRC_DIST` — i.e. exactly the saturated tail, with no need to
/// enumerate it explicitly.
fn bucket_crowding(
    sources: &[(&SourceId, f64)],
    idx: &mut SourceIndex,
    taxonomy: &Taxonomy,
    levels: &KSrcLevels,
) -> Vec<f64> {
    let n = sources.len();
    if n <= 1 {
        return sources.iter().map(|&(_, m)| m).collect();
    }

    let total_mass: f64 = sources.iter().map(|&(_, m)| m).sum();
    let saturated_level = f64::from(*levels.levels.last().unwrap());

    type Hist = [f64; MAX_K_SRC_DIST + 1];
    let mut depth_hist: HashMap<u32, Hist> = HashMap::with_capacity(sources.len() * 2);
    for &(s, m) in sources {
        let chain = idx.chain(taxonomy, s);
        for (e, &node) in chain.iter().take(MAX_K_SRC_DIST + 1).enumerate() {
            depth_hist.entry(node).or_insert([0.0; MAX_K_SRC_DIST + 1])[e] += m;
        }
    }

    sources
        .iter()
        .map(|&(s_i, mass_i)| {
            let chain_i = idx.chain(taxonomy, s_i).to_vec();
            let mut crowding = mass_i; // self term: k_src(i, i) = 1
            let mut accounted = mass_i; // mass already attributed to some (da, db)
            let mut prev_hist: Option<Hist> = None;
            for da in 0..=MAX_K_SRC_DIST {
                let Some(&node) = chain_i.get(da) else {
                    break;
                };
                let hist = depth_hist
                    .get(&node)
                    .copied()
                    .unwrap_or([0.0; MAX_K_SRC_DIST + 1]);
                for db in 0..=(MAX_K_SRC_DIST - da) {
                    if da == 0 && db == 0 {
                        continue; // j == i, already the self term above
                    }
                    let claimed_by_shallower = if db >= 1 {
                        prev_hist.map_or(0.0, |h| h[db - 1])
                    } else {
                        0.0
                    };
                    let exact = (hist[db] - claimed_by_shallower).max(0.0);
                    if exact > 0.0 {
                        let dist = da + db;
                        let level_idx = dist.saturating_sub(1).min(levels.levels.len() - 1);
                        crowding += f64::from(levels.levels[level_idx]) * exact;
                        accounted += exact;
                    }
                }
                prev_hist = Some(hist);
            }
            let saturated = (total_mass - accounted).max(0.0);
            crowding + saturated_level * saturated
        })
        .collect()
}

type DepthHist = [f64; MAX_K_SRC_DIST + 1];

/// The `depth_hist[node][e]` table `bucket_crowding` builds inline,
/// factored out so `cross_bucket_crowding` below can build one from a
/// source set *other* than the one it queries against.
fn build_depth_hist(
    sources: &[(&SourceId, f64)],
    idx: &mut SourceIndex,
    taxonomy: &Taxonomy,
) -> HashMap<u32, DepthHist> {
    let mut hist: HashMap<u32, DepthHist> = HashMap::with_capacity(sources.len() * 2);
    for &(s, m) in sources {
        let chain = idx.chain(taxonomy, s);
        for (e, &node) in chain.iter().take(MAX_K_SRC_DIST + 1).enumerate() {
            hist.entry(node).or_insert([0.0; MAX_K_SRC_DIST + 1])[e] += m;
        }
    }
    hist
}

/// `sum_j k_src(i, j) * mass_j` for the source owning `chain`, against
/// whatever source set `hist` (from `build_depth_hist`) was built
/// from — the query half of `bucket_crowding`'s trick, generalized to
/// a histogram that need not include `chain`'s own source at all.
/// `total_mass` is `hist`'s full mass, for the saturated tail.
///
/// Distance 0 (`da == db == 0`) means `hist` genuinely contains the
/// *same* `SourceId` `chain` belongs to — not a "this is me" case to
/// special-case away, since caller and histogram can be entirely
/// different cells (e.g. one atom's blend split across two anchors
/// for the same source) — so it's scored at the exact `k_src(i,i) =
/// 1.0` (`SourceIndex::k_src`'s own shortcut), not the `levels[dist -
/// 1]` lookup every other distance uses.
fn crowding_from_hist(
    chain: &[u32],
    hist: &HashMap<u32, DepthHist>,
    total_mass: f64,
    levels: &KSrcLevels,
) -> f64 {
    let saturated_level = f64::from(*levels.levels.last().unwrap());
    let mut crowding = 0.0;
    let mut accounted = 0.0;
    let mut prev_hist: Option<DepthHist> = None;
    for da in 0..=MAX_K_SRC_DIST {
        let Some(&node) = chain.get(da) else {
            break;
        };
        let node_hist = hist
            .get(&node)
            .copied()
            .unwrap_or([0.0; MAX_K_SRC_DIST + 1]);
        for db in 0..=(MAX_K_SRC_DIST - da) {
            let claimed_by_shallower = if db >= 1 {
                prev_hist.map_or(0.0, |h| h[db - 1])
            } else {
                0.0
            };
            let exact = (node_hist[db] - claimed_by_shallower).max(0.0);
            if exact > 0.0 {
                let dist = da + db;
                let weight = if dist == 0 {
                    1.0
                } else {
                    f64::from(levels.levels[dist.saturating_sub(1).min(levels.levels.len() - 1)])
                };
                crowding += weight * exact;
                accounted += exact;
            }
        }
        prev_hist = Some(node_hist);
    }
    let saturated = (total_mass - accounted).max(0.0);
    crowding + saturated_level * saturated
}

/// Cross-anchor analogue of `bucket_crowding`: given two same-sign
/// buckets on *different* anchors joined by a nonzero `k_feat` weight,
/// returns `(crowding felt by `a` due to `b`, crowding felt by `b` due
/// to `a`)` — same depth-histogram trick, just built from the other
/// side instead of from the queried set itself, in `O((|a|+|b|) *
/// MAX_K_SRC_DIST^2)` rather than the naive `O(|a|*|b|)`.
fn cross_bucket_crowding(
    a: &[(&SourceId, f64)],
    b: &[(&SourceId, f64)],
    idx: &mut SourceIndex,
    taxonomy: &Taxonomy,
    levels: &KSrcLevels,
) -> (Vec<f64>, Vec<f64>) {
    let total_a: f64 = a.iter().map(|&(_, m)| m).sum();
    let total_b: f64 = b.iter().map(|&(_, m)| m).sum();
    let hist_a = build_depth_hist(a, idx, taxonomy);
    let hist_b = build_depth_hist(b, idx, taxonomy);

    let crowd_a = a
        .iter()
        .map(|&(s, _)| {
            let chain = idx.chain(taxonomy, s).to_vec();
            crowding_from_hist(&chain, &hist_b, total_b, levels)
        })
        .collect();
    let crowd_b = b
        .iter()
        .map(|&(s, _)| {
            let chain = idx.chain(taxonomy, s).to_vec();
            crowding_from_hist(&chain, &hist_a, total_a, levels)
        })
        .collect();

    (crowd_a, crowd_b)
}

/// Self-inclusive, same-signed crowding `c_i = sum_j K(i,j) * |m_j|` for
/// every cell in `cells`, in the same order — the shared crowding pass
/// behind both `DegreeQ`'s value and `Spectral`'s per-cell breakdown
/// (`explain`'s columns need to stay comparable across modes).
///
/// Split into two parts: same-anchor pairs (where `k_feat` is always the
/// constant `1.0` diagonal, `KFeatMatrix::get`'s `a == b` fast path) go
/// through `bucket_crowding`'s `O(bucket * MAX_K_SRC_DIST)` aggregation;
/// cross-anchor pairs with an explicitly configured nonzero `k_feat`
/// weight (sparse "only non-default pairs" by `KFeatMatrix`'s own
/// contract) are handled per configured anchor pair via
/// `cross_bucket_crowding`, so the cost scales with how many pairs are
/// actually configured (a handful, from `nrs-fit`) rather than with
/// `cells.len()^2` — the naive all-pairs fallback this replaced turned
/// out to be a real 3-4x `score()` regression the moment `k_feat`
/// stopped being empty (see `kfeat_covariance`'s perf note).
/// A same-sign, same-anchor bucket entry: `(source, |mass|, original
/// index into `cells`)`.
type BucketEntry<'a> = (&'a SourceId, f64, usize);

fn all_crowdings(
    cells: &[(&SourceId, &AnchorId, f32)],
    k_feat: &KFeatMatrix,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    k_src_levels: &KSrcLevels,
) -> Vec<f64> {
    let mut crowding = vec![0.0f64; cells.len()];

    let mut buckets: HashMap<(bool, &AnchorId), Vec<BucketEntry>> = HashMap::new();
    for (i, &(source, anchor, mass)) in cells.iter().enumerate() {
        buckets
            .entry((mass.signum() > 0.0, anchor))
            .or_default()
            .push((source, f64::from(mass.abs()), i));
    }
    for entries in buckets.values() {
        let sources: Vec<(&SourceId, f64)> = entries.iter().map(|&(s, m, _)| (s, m)).collect();
        let bucket_result = bucket_crowding(&sources, idx, taxonomy, k_src_levels);
        for (&(_, _, i), c) in entries.iter().zip(&bucket_result) {
            crowding[i] = *c;
        }
    }

    for ((anchor_a, anchor_b), &kf) in &k_feat.pairs {
        if kf == 0.0 {
            continue;
        }
        for sign in [true, false] {
            let (Some(bucket_a), Some(bucket_b)) = (
                buckets.get(&(sign, anchor_a)),
                buckets.get(&(sign, anchor_b)),
            ) else {
                continue;
            };
            let a: Vec<(&SourceId, f64)> = bucket_a.iter().map(|&(s, m, _)| (s, m)).collect();
            let b: Vec<(&SourceId, f64)> = bucket_b.iter().map(|&(s, m, _)| (s, m)).collect();
            let (crowd_a, crowd_b) = cross_bucket_crowding(&a, &b, idx, taxonomy, k_src_levels);
            for (&(_, _, i), c) in bucket_a.iter().zip(&crowd_a) {
                crowding[i] += f64::from(kf) * c;
            }
            for (&(_, _, i), c) in bucket_b.iter().zip(&crowd_b) {
                crowding[i] += f64::from(kf) * c;
            }
        }
    }

    crowding
}

/// `V(f) = sum_x f(x) . phi(c(x))` (`Homogeneity::DegreeQ`, the
/// default: notes Def. "the redundancy exponent q") or its degree-1
/// alternate (`Homogeneity::Degree1`, notes §"Two knobs, and two
/// honest subtleties"), plus the per-cell breakdown behind whichever
/// is used (source, anchor, mass, crowding, contribution).
///
/// The two forms are not the same knob at different `q`: at `q = 1`
/// they coincide exactly (both reduce to the redundancy-blind `sum
/// mass`), but away from `q = 1`, `DegreeQ` scales as
/// `intensity^q` under uniformly rescaling the *whole* measure, while
/// `Degree1` scales as `intensity^1` (double every reaction, double
/// the score) by construction, at the cost of discounting redundancy
/// only *between* cells rather than within a single one — see
/// `Degree1`'s match arm below for why that trade is forced, not
/// chosen.
pub(crate) fn value(
    f: &Measure,
    k_feat: &KFeatMatrix,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    k_src_levels: &KSrcLevels,
    q: f32,
    homogeneity: Homogeneity,
) -> (f32, Vec<Contribution>) {
    // The spectral value is a whole different computation (it reads the
    // kernel's spectrum, not a per-cell crowding sum), so it branches out
    // before the additive path below rather than sharing its loop.
    if let Homogeneity::Spectral { beta } = homogeneity {
        return spectral_value(f, k_feat, taxonomy, idx, k_src_levels, beta);
    }

    let cells: Vec<(&SourceId, &crate::types::AnchorId, f32)> = f
        .iter()
        .filter(|(_, m)| **m != 0.0)
        .map(|((s, a), m)| (s, a, *m))
        .collect();

    let crowdings = all_crowdings(&cells, k_feat, taxonomy, idx, k_src_levels);

    let mut total = 0.0;
    let mut contributions = Vec::with_capacity(cells.len());
    for (&(source_i, anchor_i, mass_i), &crowding) in cells.iter().zip(&crowdings) {
        // The one nonlinearity, evaluated in log-space. DegreeQ's cell
        // term is `m * c^(q-1)`; Degree1's is `m * (c/|m|)^(q-1)`,
        // normalising crowding by the cell's own mass first (a
        // scale-free "how surrounded am I relative to my own size"
        // ratio, always 1 for a cell with no same-signed neighbour — the
        // exact-degree-1 property that makes a lone cell contribute its
        // mass linearly for every q, since a single-point measure can't
        // be told apart from "rescale everything").
        //
        // For q<1 both forms are bounded by |m|^q (crowding >= |m| via
        // the self term K(i,i)=1), tending to 0 as m -> 0. But the naive
        // `c.powf(q-1)` overflows to `inf` for the tiny crowding PowQ
        // yields at low q *before* the outer `* m` brings it back into
        // range — which the old code caught with an `is_finite()` guard
        // that then floored the whole (small but real) term to 0.
        // Writing it as `sign(m) * exp(ln|m| + (q-1)*ln arg)` keeps every
        // intermediate near the bounded final magnitude, so the true
        // value survives and no underflow-fallback is needed.
        let m_abs = f64::from(mass_i.abs());
        let contribution = if m_abs == 0.0 || crowding == 0.0 {
            0.0
        } else {
            let ln_arg = match homogeneity {
                Homogeneity::DegreeQ => crowding.ln(),
                Homogeneity::Degree1 => crowding.ln() - m_abs.ln(),
                // Handled by the early return above; never reached here.
                Homogeneity::Spectral { .. } => unreachable!(),
            };
            let ln_contrib = m_abs.ln() + f64::from(q - 1.0) * ln_arg;
            let val = f64::from(mass_i.signum()) * ln_contrib.exp();
            // Defensive only: for the q<1 discount regime `ln_contrib` is
            // bounded above by `q*ln|m|`, so `exp` cannot overflow for
            // any mass/crowding that actually occurs; this still floors a
            // genuine over/underflow (e.g. an absurd q) to 0 rather than
            // letting NaN/inf poison the total.
            if val.is_finite() {
                val as f32
            } else {
                0.0
            }
        };
        total += contribution;
        contributions.push(Contribution {
            source: source_i.clone(),
            anchor: anchor_i.clone(),
            mass: mass_i,
            crowding: crowding as f32,
            contribution,
        });
    }
    (total, contributions)
}

/// Anchor-id-to-compact-index resolution for `k_feat`, built once per
/// `spectral_value` call and reused across both sign classes:
/// `KFeatMatrix::get`'s `BTreeMap<(AnchorId, AnchorId), f32>` has no
/// borrowed-key query path for a tuple, so every lookup on the raw
/// matrix clones both `AnchorId` `String`s — free to skip when `pairs`
/// is empty (`get`'s own fast path), but `spectral_class`'s `G` build
/// calls it for every cross-anchor cell pair, `O(cells^2)` per entry;
/// once `pairs` is actually populated that clone cost dominates.
/// Building this once per entry (not once per `score()` call — the
/// index itself is tiny, `O(pairs.len())`, versus the per-entry `G`
/// build it feeds) turns every later lookup into plain array indexing.
struct KFeatIndex {
    index: HashMap<AnchorId, u32>,
    /// `n * n`, row-major; only anchors that appear in `k_feat.pairs`
    /// are indexed at all (any other anchor can't have a nonzero
    /// weight with anything, by `KFeatMatrix`'s own sparse contract).
    matrix: Vec<f32>,
    n: usize,
}

impl KFeatIndex {
    fn build(k_feat: &KFeatMatrix) -> Self {
        let mut index: HashMap<AnchorId, u32> = HashMap::new();
        for (a, b) in k_feat.pairs.keys() {
            let next = index.len() as u32;
            index.entry(a.clone()).or_insert(next);
            let next = index.len() as u32;
            index.entry(b.clone()).or_insert(next);
        }
        let n = index.len();
        let mut matrix = vec![0.0f32; n * n];
        for ((a, b), &kf) in &k_feat.pairs {
            let ia = index[a] as usize;
            let ib = index[b] as usize;
            matrix[ia * n + ib] = kf;
            matrix[ib * n + ia] = kf;
        }
        Self { index, matrix, n }
    }

    /// Same contract as `KFeatMatrix::get`, without ever cloning an
    /// `AnchorId`.
    fn get(&self, a: &AnchorId, b: &AnchorId) -> f32 {
        if a == b {
            return 1.0;
        }
        let (Some(&ia), Some(&ib)) = (self.index.get(a), self.index.get(b)) else {
            return 0.0;
        };
        self.matrix[ia as usize * self.n + ib as usize]
    }
}

/// `Homogeneity::Spectral`: the bounded-slope spectral redundancy value
/// (redesign-tex `spectral-bounded-slope.tex`). Rather than the per-cell
/// sum `sum_i m_i c_i^{q-1}`, score the entry through the spectrum of its
/// mass-weighted kernel `G = M^{1/2} K M^{1/2}` (`G_ij =
/// sqrt(|m_i||m_j|) K_ij`), with the concave, bounded-origin-slope gauge
/// `g(t) = beta * ln(1 + t/beta)`: `V = sum_k g(sigma_k)`.
///
/// This is monotone (adding mass only raises eigenvalues, by Weyl),
/// exactly splitting-invariant (it reads `f` only through `G`, and
/// coincident pieces rebuild the same `G`), discounts redundancy with
/// the correct sign (`g` concave => the value is Schur-concave in the
/// spectrum), and — unlike the Schatten form `(tr G^p)^{1/p}` —
/// converges under fixed-spread refinement, because `g` has finite slope
/// at the origin.
///
/// PSD-ness: the construction needs `K >= 0`, which the live
/// `k_feat`/`k_src` tables do not guarantee. Rather than restrict those
/// tables, we take the nearest-PSD projection of `G` — clip negative
/// eigenvalues to `0` — which is the minimal-distortion fix and free
/// here, since the eigendecomposition is already computed for the value
/// and its per-cell breakdown. (A PSD-by-construction kernel would avoid
/// even that clip; left as a follow-up, see the note's open questions.)
///
/// Signs: opposite-signed cells are never mutually redundant (this
/// module's header), so positive and negative mass are scored as two
/// independent PSD operators and subtracted, keeping monotonicity within
/// each sign class.
fn spectral_value(
    f: &Measure,
    k_feat: &KFeatMatrix,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    k_src_levels: &KSrcLevels,
    beta: f32,
) -> (f32, Vec<Contribution>) {
    let beta = f64::from(beta).max(f64::MIN_POSITIVE);
    let cells: Vec<(&SourceId, &AnchorId, f32)> = f
        .iter()
        .filter(|(_, m)| **m != 0.0)
        .map(|((s, a), m)| (s, a, *m))
        .collect();
    let k_feat_idx = KFeatIndex::build(k_feat);

    // Per-cell contribution, indexed like `cells`; filled from each sign
    // class's spectral attribution, then emitted in `cells` order so the
    // breakdown is deterministic and sums to the value.
    let mut contrib_by_cell = vec![0.0f64; cells.len()];
    for sign in [1.0f32, -1.0] {
        let idxs: Vec<usize> = (0..cells.len())
            .filter(|&i| cells[i].2.signum() == sign)
            .collect();
        let attrib = spectral_class(
            &cells,
            &idxs,
            &k_feat_idx,
            taxonomy,
            idx,
            k_src_levels,
            beta,
        );
        // Positive class adds its log-volume, negative class subtracts.
        for (local, &global) in idxs.iter().enumerate() {
            contrib_by_cell[global] = f64::from(sign) * attrib[local];
        }
    }

    // Crowding is kept for the breakdown only (it does not drive the
    // spectral value): the same self-inclusive, same-signed crowding
    // `sum_j K_ij |m_j|` that DegreeQ reports, so `explain`'s columns
    // stay comparable across scoring modes.
    let crowdings = all_crowdings(&cells, k_feat, taxonomy, idx, k_src_levels);

    let mut total = 0.0f64;
    let mut contributions = Vec::with_capacity(cells.len());
    for (i, (&(source_i, anchor_i, mass_i), &crowding)) in cells.iter().zip(&crowdings).enumerate()
    {
        let contribution = contrib_by_cell[i] as f32;
        total += f64::from(contribution);
        contributions.push(Contribution {
            source: source_i.clone(),
            anchor: anchor_i.clone(),
            mass: mass_i,
            crowding: crowding as f32,
            contribution,
        });
    }
    (total as f32, contributions)
}

/// Score one sign class: build `G` over the cells in `idxs`,
/// eigendecompose, and return each cell's share of `V = sum_k
/// g(sigma_k)`. Cell `i`'s share is `[g(G)]_ii = sum_k g(sigma_k)
/// vec_k[i]^2`; these sum to `V` exactly (each eigenvector is a unit
/// vector), so the trace decomposition is an exact, order-independent
/// per-cell attribution of the log-volume. Returns one value per entry
/// of `idxs`, in `idxs` order.
fn spectral_class(
    cells: &[(&SourceId, &AnchorId, f32)],
    idxs: &[usize],
    k_feat_idx: &KFeatIndex,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    k_src_levels: &KSrcLevels,
    beta: f64,
) -> Vec<f64> {
    let n = idxs.len();
    if n == 0 {
        return Vec::new();
    }
    let g = build_spectral_kernel(cells, idxs, k_feat_idx, taxonomy, idx, k_src_levels);

    // `faer` has enough fixed per-call overhead (thread-pool/parallelism
    // check, scratch allocation, SIMD kernel dispatch) that it's slower
    // than the hand-rolled solver below this size in practice — measured
    // *slower* than `jacobi_eigen` for n<=4, breakeven around n=8 (see
    // linalg.rs's `householder_ql_vs_jacobi_timing`). Since most entries'
    // per-sign-class kernels are small and only a handful are large, a
    // blanket faer swap was a net *regression* end to end; this dispatch
    // sends only the few large kernels to it.
    #[cfg(feature = "faer-eigen")]
    let (vals, vecs) = if n >= 64 {
        faer_eigen(g)
    } else {
        householder_ql_eigen(g)
    };
    #[cfg(not(feature = "faer-eigen"))]
    let (vals, vecs) = householder_ql_eigen(g);
    // g(t) = beta ln(1 + t/beta), clipping negative eigenvalues to 0
    // (the nearest-PSD projection). g(0) = 0, so clipped modes drop out.
    let gv: Vec<f64> = vals
        .iter()
        .map(|&sigma| beta * (1.0 + sigma.max(0.0) / beta).ln())
        .collect();
    (0..n)
        .map(|i| (0..n).map(|k| gv[k] * vecs[i][k] * vecs[i][k]).sum())
        .collect()
}

/// `G_ab = sqrt(|m_a| |m_b|) * K(a, b)`, `K = k_feat . k_src`, over the
/// cells in `idxs` — shared by `spectral_class` (needs eigenvectors,
/// for `explain()`'s per-cell breakdown) and `spectral_class_value`
/// (needs only eigenvalues, for `score()`, which discards that
/// breakdown entirely). `k_feat` is checked first and `k_src` skipped
/// when it's already zero: with only a handful of anchors and (in the
/// real data) an unpopulated `k_feat`, most `(anchor_i, anchor_j)`
/// pairs are zero, so this avoids the bulk of `k_src`'s (still
/// non-free, even memoized) chain lookups for pairs whose product is
/// zero regardless.
fn build_spectral_kernel(
    cells: &[(&SourceId, &AnchorId, f32)],
    idxs: &[usize],
    k_feat_idx: &KFeatIndex,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    k_src_levels: &KSrcLevels,
) -> Vec<Vec<f64>> {
    let n = idxs.len();
    let mut g = vec![vec![0.0f64; n]; n];
    for a in 0..n {
        let (s_a, an_a, m_a) = cells[idxs[a]];
        for b in a..n {
            let (s_b, an_b, m_b) = cells[idxs[b]];
            let kf = k_feat_idx.get(an_a, an_b);
            let g_ab = if kf == 0.0 {
                0.0
            } else {
                let k = f64::from(kf) * f64::from(idx.k_src(s_a, s_b, taxonomy, k_src_levels));
                (f64::from(m_a.abs()) * f64::from(m_b.abs())).sqrt() * k
            };
            g[a][b] = g_ab;
            g[b][a] = g_ab;
        }
    }
    g
}

/// Same per-sign-class computation as `spectral_class`, but only the
/// scalar `V_class = sum_k g(sigma_k)` — `score()`'s only use for
/// `Spectral` mode, since it discards `explain()`'s per-cell breakdown
/// entirely (`lib.rs::score()`'s `let (v, _) = ...`). `spectral_class`'s
/// per-cell attribution needs the full eigendecomposition (accumulated
/// eigenvectors), but the scalar total is exactly a sum over
/// eigenvalues alone, so this goes through the cheaper
/// `householder_ql_eigenvalues` instead - see its docs for why that's
/// exact, not approximate.
fn spectral_class_value(
    cells: &[(&SourceId, &AnchorId, f32)],
    idxs: &[usize],
    k_feat_idx: &KFeatIndex,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    k_src_levels: &KSrcLevels,
    beta: f64,
) -> f64 {
    if idxs.is_empty() {
        return 0.0;
    }
    let g = build_spectral_kernel(cells, idxs, k_feat_idx, taxonomy, idx, k_src_levels);
    householder_ql_eigenvalues(g)
        .iter()
        .map(|&sigma| beta * (1.0 + sigma.max(0.0) / beta).ln())
        .sum()
}

/// Same contract as `spectral_value`, but only the scalar `V` (see
/// `spectral_class_value`'s docs for why `score()` never needs the
/// per-cell breakdown `spectral_value` builds alongside it).
fn spectral_value_scalar(
    f: &Measure,
    k_feat: &KFeatMatrix,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    k_src_levels: &KSrcLevels,
    beta: f32,
) -> f32 {
    let beta = f64::from(beta).max(f64::MIN_POSITIVE);
    let cells: Vec<(&SourceId, &AnchorId, f32)> = f
        .iter()
        .filter(|(_, m)| **m != 0.0)
        .map(|((s, a), m)| (s, a, *m))
        .collect();
    let k_feat_idx = KFeatIndex::build(k_feat);

    let mut total = 0.0f64;
    for sign in [1.0f32, -1.0] {
        let idxs: Vec<usize> = (0..cells.len())
            .filter(|&i| cells[i].2.signum() == sign)
            .collect();
        let v_class = spectral_class_value(
            &cells,
            &idxs,
            &k_feat_idx,
            taxonomy,
            idx,
            k_src_levels,
            beta,
        );
        total += f64::from(sign) * v_class;
    }
    total as f32
}

/// Same contract as `value`, but only ever returns the scalar `V` —
/// `score()`'s only use (`explain()` keeps calling `value` for the
/// per-cell `Contribution` breakdown). For `DegreeQ`/`Degree1` this is
/// just `value` with its contributions dropped (no extra work to skip:
/// `all_crowdings`'s output is needed for `V` either way). For
/// `Spectral` it's a real saving — see `spectral_value_scalar`.
pub(crate) fn value_scalar(
    f: &Measure,
    k_feat: &KFeatMatrix,
    taxonomy: &Taxonomy,
    idx: &mut SourceIndex,
    k_src_levels: &KSrcLevels,
    q: f32,
    homogeneity: Homogeneity,
) -> f32 {
    if let Homogeneity::Spectral { beta } = homogeneity {
        return spectral_value_scalar(f, k_feat, taxonomy, idx, k_src_levels, beta);
    }
    value(f, k_feat, taxonomy, idx, k_src_levels, q, homogeneity).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AnchorId;
    use std::collections::BTreeMap;

    // ---- bucket_crowding: cross-checked against the naive O(n^2)
    // all-pairs `k_src` sum it replaces, across randomized taxonomy
    // shapes (empty, chains, branching trees) and bucket sizes. ----

    /// The straightforward `O(n^2)` computation `bucket_crowding`
    /// optimizes: `crowding_i = sum_j k_src(i, j) * mass_j`. Kept only
    /// for tests, as the correctness oracle.
    fn brute_force_crowding(
        sources: &[(&SourceId, f64)],
        idx: &mut SourceIndex,
        taxonomy: &Taxonomy,
        levels: &KSrcLevels,
    ) -> Vec<f64> {
        sources
            .iter()
            .map(|&(s_i, _)| {
                sources
                    .iter()
                    .map(|&(s_j, m_j)| f64::from(idx.k_src(s_i, s_j, taxonomy, levels)) * m_j)
                    .sum()
            })
            .collect()
    }

    /// A small xorshift PRNG, deterministic across runs, no extra dep.
    struct Xorshift(u64);
    impl Xorshift {
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn next_range(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
        fn next_f32(&mut self) -> f32 {
            (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    /// A random forest over `n` labeled nodes: each node either has no
    /// parent, or a parent chosen from *lower-numbered* nodes only (so
    /// the result is acyclic by construction, no cycle-guard needed to
    /// interpret it).
    fn random_taxonomy(rng: &mut Xorshift, n: usize) -> Taxonomy {
        let mut taxonomy = Taxonomy::default();
        for i in 1..n {
            // ~60% chance of a parent, chosen uniformly among 0..i.
            if rng.next_range(10) < 6 {
                let p = rng.next_range(i);
                taxonomy
                    .parent
                    .insert(SourceId(format!("n{i}")), SourceId(format!("n{p}")));
            }
        }
        taxonomy
    }

    #[test]
    fn bucket_crowding_matches_brute_force_on_random_taxonomies() {
        let mut rng = Xorshift(0xD1CE_B00C_1234_5678);
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };

        for &n in &[1usize, 2, 3, 5, 8, 16, 40] {
            for trial in 0..5 {
                let taxonomy = random_taxonomy(&mut rng, n);
                let ids: Vec<SourceId> = (0..n).map(|i| SourceId(format!("n{i}"))).collect();
                let masses: Vec<f64> = (0..n).map(|_| f64::from(rng.next_f32()) + 0.01).collect();
                let sources: Vec<(&SourceId, f64)> =
                    ids.iter().zip(masses.iter().copied()).collect();

                let mut idx_a = SourceIndex::new();
                let fast = bucket_crowding(&sources, &mut idx_a, &taxonomy, &levels);
                let mut idx_b = SourceIndex::new();
                let brute = brute_force_crowding(&sources, &mut idx_b, &taxonomy, &levels);

                assert_eq!(fast.len(), brute.len());
                for (i, (f, b)) in fast.iter().zip(&brute).enumerate() {
                    assert!(
                        (f - b).abs() < 1e-9 * (1.0 + b.abs()),
                        "n={n} trial={trial} i={i}: fast={f} brute={b}"
                    );
                }
            }
        }
    }

    #[test]
    fn bucket_crowding_matches_brute_force_with_a_deep_chain() {
        // A single deep chain (n0 <- n1 <- ... <- n_{k-1}) exercises
        // distances well past MAX_K_SRC_DIST, so both the exact-distance
        // table entries and the saturated tail get real coverage.
        let mut taxonomy = Taxonomy::default();
        let n = 20;
        for i in 1..n {
            taxonomy
                .parent
                .insert(SourceId(format!("n{i}")), SourceId(format!("n{}", i - 1)));
        }
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        let ids: Vec<SourceId> = (0..n).map(|i| SourceId(format!("n{i}"))).collect();
        let masses: Vec<f64> = (0..n).map(|i| 1.0 + i as f64 * 0.1).collect();
        let sources: Vec<(&SourceId, f64)> = ids.iter().zip(masses.iter().copied()).collect();

        let mut idx_a = SourceIndex::new();
        let fast = bucket_crowding(&sources, &mut idx_a, &taxonomy, &levels);
        let mut idx_b = SourceIndex::new();
        let brute = brute_force_crowding(&sources, &mut idx_b, &taxonomy, &levels);
        for (i, (f, b)) in fast.iter().zip(&brute).enumerate() {
            assert!(
                (f - b).abs() < 1e-9 * (1.0 + b.abs()),
                "i={i}: fast={f} brute={b}"
            );
        }
    }

    #[test]
    fn bucket_crowding_matches_brute_force_on_a_wide_star() {
        // One shared parent with many direct children (a franchise with
        // many top-level characters): heavy reuse of the same ancestor
        // node across sources, exercising depth_hist's aggregation.
        let mut taxonomy = Taxonomy::default();
        let n = 30;
        for i in 1..n {
            taxonomy
                .parent
                .insert(SourceId(format!("n{i}")), SourceId("n0".into()));
        }
        let levels = KSrcLevels {
            levels: [0.9, 0.6, 0.4, 0.2, 0.05],
        };
        let ids: Vec<SourceId> = (0..n).map(|i| SourceId(format!("n{i}"))).collect();
        let masses: Vec<f64> = (0..n).map(|_| 2.0).collect();
        let sources: Vec<(&SourceId, f64)> = ids.iter().zip(masses.iter().copied()).collect();

        let mut idx_a = SourceIndex::new();
        let fast = bucket_crowding(&sources, &mut idx_a, &taxonomy, &levels);
        let mut idx_b = SourceIndex::new();
        let brute = brute_force_crowding(&sources, &mut idx_b, &taxonomy, &levels);
        for (i, (f, b)) in fast.iter().zip(&brute).enumerate() {
            assert!(
                (f - b).abs() < 1e-9 * (1.0 + b.abs()),
                "i={i}: fast={f} brute={b}"
            );
        }
    }

    #[test]
    fn bucket_crowding_handles_trivial_sizes() {
        let idx = &mut SourceIndex::new();
        let tax = Taxonomy::default();
        let levels = KSrcLevels { levels: [1.0; 5] };
        assert_eq!(bucket_crowding(&[], idx, &tax, &levels), Vec::<f64>::new());

        let a = SourceId("a".into());
        assert_eq!(bucket_crowding(&[(&a, 3.0)], idx, &tax, &levels), vec![3.0]);
    }

    /// Brute-force `all_crowdings` oracle: the naive `O(cells^2)`
    /// all-pairs formula `cross_bucket_crowding` replaces — the
    /// correctness reference for the (much less obviously correct)
    /// cross-anchor bucketed path.
    fn brute_force_all_crowdings(
        cells: &[(&SourceId, &AnchorId, f32)],
        k_feat: &KFeatMatrix,
        taxonomy: &Taxonomy,
        idx: &mut SourceIndex,
        levels: &KSrcLevels,
    ) -> Vec<f64> {
        cells
            .iter()
            .map(|&(s_i, a_i, m_i)| {
                cells
                    .iter()
                    .filter(|item| item.2.signum() == m_i.signum())
                    .map(|&(s_j, a_j, m_j)| {
                        let kf = if a_i == a_j {
                            1.0
                        } else {
                            k_feat.get(a_i, a_j)
                        };
                        f64::from(kf)
                            * f64::from(idx.k_src(s_i, s_j, taxonomy, levels))
                            * f64::from(m_j.abs())
                    })
                    .sum()
            })
            .collect()
    }

    #[test]
    fn all_crowdings_matches_brute_force_with_populated_k_feat() {
        let mut rng = Xorshift(0xFEED_FACE_1234_5678);
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        let anchors: Vec<AnchorId> = (0..4).map(|i| AnchorId(format!("a{i}"))).collect();

        for &n_sources in &[2usize, 5, 12, 30] {
            for trial in 0..5 {
                let taxonomy = random_taxonomy(&mut rng, n_sources);
                let source_ids: Vec<SourceId> =
                    (0..n_sources).map(|i| SourceId(format!("n{i}"))).collect();

                // Each source gets 1-2 cells at *distinct* anchors
                // (real `Measure`s are keyed by `(source, anchor)`, so
                // that pair is never duplicated) - including the same
                // source landing in two different anchors (a blend
                // split), which is exactly the case
                // `crowding_from_hist`'s dist==0 special case exists for.
                let mut owned_cells: Vec<(SourceId, AnchorId, f32)> = Vec::new();
                for s in &source_ids {
                    let n_cells = 1 + rng.next_range(2);
                    let first = rng.next_range(anchors.len());
                    for k in 0..n_cells {
                        let anchor_idx = (first + k) % anchors.len();
                        let anchor = anchors[anchor_idx].clone();
                        let sign = if rng.next_range(2) == 0 { 1.0 } else { -1.0 };
                        let mass = sign * (rng.next_f32() + 0.05);
                        owned_cells.push((s.clone(), anchor, mass));
                    }
                }
                let cells: Vec<(&SourceId, &AnchorId, f32)> =
                    owned_cells.iter().map(|(s, a, m)| (s, a, *m)).collect();

                // A handful of random nonzero cross-anchor k_feat pairs.
                let mut k_feat = KFeatMatrix::default();
                for i in 0..anchors.len() {
                    for j in (i + 1)..anchors.len() {
                        if rng.next_range(2) == 0 {
                            k_feat
                                .pairs
                                .insert((anchors[i].clone(), anchors[j].clone()), rng.next_f32());
                        }
                    }
                }

                let mut idx_a = SourceIndex::new();
                let fast = all_crowdings(&cells, &k_feat, &taxonomy, &mut idx_a, &levels);
                let mut idx_b = SourceIndex::new();
                let brute =
                    brute_force_all_crowdings(&cells, &k_feat, &taxonomy, &mut idx_b, &levels);

                assert_eq!(fast.len(), brute.len());
                for (i, (f, b)) in fast.iter().zip(&brute).enumerate() {
                    assert!(
                        (f - b).abs() < 1e-6 * (1.0 + b.abs()),
                        "n_sources={n_sources} trial={trial} i={i}: fast={f} brute={b}"
                    );
                }
            }
        }
    }

    #[test]
    fn opposite_sign_masses_do_not_crowd_each_other() {
        let anchor = AnchorId("x".into());
        let k_feat = KFeatMatrix::default();
        let tax = Taxonomy::default();
        // All levels 1.0 so any two distinct sources are as related as
        // possible; this isolates the sign gate from k_src.
        let levels = KSrcLevels { levels: [1.0; 5] };

        let mut f = Measure::new();
        f.insert((SourceId("pos".into()), anchor.clone()), 2.0);
        f.insert((SourceId("neg".into()), anchor.clone()), -2.0);

        let (_, contributions) = value(
            &f,
            &k_feat,
            &tax,
            &mut SourceIndex::new(),
            &levels,
            0.5,
            Homogeneity::DegreeQ,
        );
        for c in &contributions {
            assert_eq!(
                c.crowding, 2.0,
                "opposite-sign cell must not add to crowding"
            );
        }
    }

    #[test]
    fn vanishingly_tiny_mass_contributes_zero_not_infinity() {
        // Simulates what RelationScaling::PowQ can produce at very low
        // q: an isolated cell whose mass has been diluted down near
        // f32's precision floor. The true limit as mass -> 0 is 0, not
        // `inf` from crowding^(q-1) blowing up.
        let anchor = AnchorId("x".into());
        let k_feat = KFeatMatrix::default();
        let tax = Taxonomy::default();
        let levels = KSrcLevels { levels: [1.0; 5] };

        for &tiny in &[1e-30f32, 1e-38, 1e-40, 1e-44] {
            let mut f = Measure::new();
            f.insert((SourceId("s".into()), anchor.clone()), tiny);
            let (v, contributions) = value(
                &f,
                &k_feat,
                &tax,
                &mut SourceIndex::new(),
                &levels,
                0.1,
                Homogeneity::DegreeQ,
            );
            assert!(v.is_finite(), "tiny={tiny}: V={v}");
            for c in &contributions {
                assert!(
                    c.contribution.is_finite(),
                    "tiny={tiny}: contribution={}",
                    c.contribution
                );
            }
        }
    }

    #[test]
    fn tiny_isolated_mass_keeps_its_true_value_in_log_space() {
        // The log-space evaluation's payoff: a lone cell whose mass has
        // been driven near f32's subnormal floor (what PowQ does at low
        // q) contributes its *true* `|m|^q`, not the 0 the old
        // `is_finite()` guard produced when `crowding^(q-1)` overflowed.
        let anchor = AnchorId("x".into());
        let k_feat = KFeatMatrix::default();
        let tax = Taxonomy::default();
        let levels = KSrcLevels { levels: [1.0; 5] };
        let q = 0.1f32;

        for &tiny in &[1e-20f32, 1e-30, 1e-38, 1e-42] {
            let mut f = Measure::new();
            f.insert((SourceId("s".into()), anchor.clone()), tiny);
            let (v, _) = value(
                &f,
                &k_feat,
                &tax,
                &mut SourceIndex::new(),
                &levels,
                q,
                Homogeneity::DegreeQ,
            );
            let expect = tiny.powf(q); // isolated: c = m, so m*m^(q-1) = m^q
            assert!(v > 0.0, "tiny={tiny}: floored to {v}");
            let rel = (v - expect).abs() / expect;
            assert!(
                rel < 1e-3,
                "tiny={tiny}: got {v}, want {expect} (rel {rel})"
            );
        }
    }

    #[test]
    fn identical_source_is_maximally_related() {
        let s = SourceId("same".into());
        let tax = Taxonomy::default();
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        assert_eq!(SourceIndex::new().k_src(&s, &s, &tax, &levels), 1.0);
    }

    #[test]
    fn unrelated_sources_use_last_level() {
        let a = SourceId("a".into());
        let b = SourceId("b".into());
        let tax = Taxonomy::default();
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        assert_eq!(SourceIndex::new().k_src(&a, &b, &tax, &levels), 0.0);
    }

    /// The unbounded formula `k_src` used before the `levels.len()`-
    /// bounded scan: full ancestor chains, no early cutoff. Kept only
    /// as the correctness oracle for chains deeper than `levels.len()`,
    /// where the bounded search's early exit is actually exercised.
    fn brute_force_k_src(
        a: &SourceId,
        b: &SourceId,
        taxonomy: &Taxonomy,
        levels: &KSrcLevels,
    ) -> f32 {
        if a == b {
            return 1.0;
        }
        fn full_chain(taxonomy: &Taxonomy, id: &SourceId) -> Vec<SourceId> {
            let mut chain = vec![id.clone()];
            let mut current = id.clone();
            while let Some(parent) = taxonomy.parent.get(&current) {
                if chain.contains(parent) {
                    break;
                }
                chain.push(parent.clone());
                current = parent.clone();
            }
            chain
        }
        let chain_a = full_chain(taxonomy, a);
        let chain_b = full_chain(taxonomy, b);
        for (da, node) in chain_a.iter().enumerate() {
            if let Some(db) = chain_b.iter().position(|n| n == node) {
                let dist = da + db;
                let idx = dist.saturating_sub(1).min(levels.levels.len() - 1);
                return levels.levels[idx];
            }
        }
        *levels.levels.last().unwrap()
    }

    #[test]
    fn k_src_bounded_scan_matches_unbounded_brute_force_on_deep_chains() {
        let mut rng = Xorshift(0x600D_DEED_C0FF_EE42);
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        // A single deep chain, well past `levels.len()`, so pairs at
        // every combined distance - short, exactly at the boundary,
        // and saturated - all get exercised.
        let mut taxonomy = Taxonomy::default();
        let n = 15;
        for i in 1..n {
            taxonomy
                .parent
                .insert(SourceId(format!("n{i}")), SourceId(format!("n{}", i - 1)));
        }
        let ids: Vec<SourceId> = (0..n).map(|i| SourceId(format!("n{i}"))).collect();

        for _ in 0..50 {
            let a = &ids[rng.next_range(n)];
            let b = &ids[rng.next_range(n)];
            let mut idx = SourceIndex::new();
            let fast = idx.k_src(a, b, &taxonomy, &levels);
            let brute = brute_force_k_src(a, b, &taxonomy, &levels);
            assert_eq!(fast, brute, "a={a:?} b={b:?}: fast={fast} brute={brute}");
        }
    }

    #[test]
    fn same_feeling_twice_saturates_below_two_distinct() {
        // Notes scenario ("same feeling twice vs. two distinct
        // feelings"): two feelings of size `a`, q = 0.5. Same
        // (source, anchor) twice: V = sqrt(2a) * ... ; two distinct
        // cells: V = 2*sqrt(a). The former must be strictly smaller.
        let a = 3.0f32;
        let q = 0.5;
        let k_feat = KFeatMatrix::default();
        let tax = Taxonomy::default();
        // Unrelated sources (no shared ancestor) fall back to the last
        // level, so two distinct sources here are ~fully unredundant.
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        let anchor = AnchorId("x".into());
        let source = SourceId("s".into());

        let mut same_cell = Measure::new();
        *same_cell
            .entry((source.clone(), anchor.clone()))
            .or_insert(0.0) += a;
        *same_cell
            .entry((source.clone(), anchor.clone()))
            .or_insert(0.0) += a;
        let (v_same, _) = value(
            &same_cell,
            &k_feat,
            &tax,
            &mut SourceIndex::new(),
            &levels,
            q,
            Homogeneity::DegreeQ,
        );

        let mut distinct_cells: BTreeMap<_, _> = BTreeMap::new();
        distinct_cells.insert((SourceId("s1".into()), anchor.clone()), a);
        distinct_cells.insert((SourceId("s2".into()), anchor.clone()), a);
        let (v_distinct, _) = value(
            &distinct_cells,
            &k_feat,
            &tax,
            &mut SourceIndex::new(),
            &levels,
            q,
            Homogeneity::DegreeQ,
        );

        assert!(v_same < v_distinct, "{v_same} should be < {v_distinct}");
    }

    fn two_related_cells(a: f32, anchor: &AnchorId) -> Measure {
        let mut f = Measure::new();
        f.insert((SourceId("s1".into()), anchor.clone()), a);
        f.insert((SourceId("s2".into()), anchor.clone()), a);
        f
    }

    #[test]
    fn degree1_and_degreeq_agree_at_q_one() {
        let k_feat = KFeatMatrix::default();
        let tax = Taxonomy::default();
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        let anchor = AnchorId("x".into());
        let f = two_related_cells(3.0, &anchor);

        let (v_q, _) = value(
            &f,
            &k_feat,
            &tax,
            &mut SourceIndex::new(),
            &levels,
            1.0,
            Homogeneity::DegreeQ,
        );
        let (v_1, _) = value(
            &f,
            &k_feat,
            &tax,
            &mut SourceIndex::new(),
            &levels,
            1.0,
            Homogeneity::Degree1,
        );
        assert!((v_q - v_1).abs() < 1e-5, "{v_q} vs {v_1}");
    }

    #[test]
    fn degree1_scales_exactly_linearly() {
        let k_feat = KFeatMatrix::default();
        let tax = Taxonomy::default();
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        let anchor = AnchorId("x".into());
        let f = two_related_cells(3.0, &anchor);
        let scaled = two_related_cells(3.0 * 7.0, &anchor);

        let (v, _) = value(
            &f,
            &k_feat,
            &tax,
            &mut SourceIndex::new(),
            &levels,
            0.5,
            Homogeneity::Degree1,
        );
        let (v_scaled, _) = value(
            &scaled,
            &k_feat,
            &tax,
            &mut SourceIndex::new(),
            &levels,
            0.5,
            Homogeneity::Degree1,
        );
        assert!(
            (v_scaled - 7.0 * v).abs() < 1e-3,
            "{v_scaled} vs {}",
            7.0 * v
        );
    }

    #[test]
    fn degree1_lone_cell_is_exactly_its_own_mass() {
        // Hard consequence of exact degree-1 homogeneity, not a
        // shortcut: a measure supported at one point can't be told
        // apart from "uniformly rescale everything".
        let k_feat = KFeatMatrix::default();
        let tax = Taxonomy::default();
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        let anchor = AnchorId("x".into());
        let mut f = Measure::new();
        f.insert((SourceId("s".into()), anchor), 5.0);

        for &q in &[0.1f32, 0.5, 1.0, 2.0] {
            let (v, _) = value(
                &f,
                &k_feat,
                &tax,
                &mut SourceIndex::new(),
                &levels,
                q,
                Homogeneity::Degree1,
            );
            assert!((v - 5.0).abs() < 1e-5, "q={q}: {v}");
        }
    }

    #[test]
    fn kfeat_index_matches_kfeat_matrix_get() {
        let mut rng = Xorshift(0xA11C_E5FE_D000_1234);
        let anchors: Vec<AnchorId> = (0..6).map(|i| AnchorId(format!("a{i}"))).collect();

        for trial in 0..10 {
            let mut k_feat = KFeatMatrix::default();
            for i in 0..anchors.len() {
                for j in (i + 1)..anchors.len() {
                    if rng.next_range(2) == 0 {
                        k_feat
                            .pairs
                            .insert((anchors[i].clone(), anchors[j].clone()), rng.next_f32());
                    }
                }
            }
            let k_feat_idx = KFeatIndex::build(&k_feat);

            for i in 0..anchors.len() {
                for j in 0..anchors.len() {
                    let want = k_feat.get(&anchors[i], &anchors[j]);
                    let got = k_feat_idx.get(&anchors[i], &anchors[j]);
                    assert_eq!(
                        want, got,
                        "trial={trial} i={i} j={j}: matrix={want} index={got}"
                    );
                }
            }

            // An anchor never mentioned in `pairs` at all must still
            // resolve to 0 against everything (not panic on a missing
            // index entry).
            let stranger = AnchorId("never-seen".into());
            for a in &anchors {
                assert_eq!(k_feat_idx.get(&stranger, a), 0.0);
            }
        }
    }

    // ---- Homogeneity::Spectral (redesign-tex spectral-bounded-slope) ----

    /// `g(t) = beta ln(1 + t/beta)`, the spectral gauge, for expectations.
    fn g(t: f64, beta: f64) -> f64 {
        beta * (1.0 + t / beta).ln()
    }

    fn spectral(f: &Measure, k_feat: &KFeatMatrix, levels: &KSrcLevels, beta: f32) -> f32 {
        let tax = Taxonomy::default();
        value(
            f,
            k_feat,
            &tax,
            &mut SourceIndex::new(),
            levels,
            0.0,
            Homogeneity::Spectral { beta },
        )
        .0
    }

    #[test]
    fn spectral_isolated_atom_scores_g_of_its_mass() {
        // A lone cell has G = [m], one eigenvalue m, so V = g(m) exactly
        // (not the m^q drift DegreeQ has, nor the m of Degree1).
        let levels = KSrcLevels { levels: [1.0; 5] };
        let k_feat = KFeatMatrix::default();
        let beta = 2.0f32;
        for &m in &[0.5f32, 1.0, 4.0, 20.0] {
            let mut f = Measure::new();
            f.insert((SourceId("s".into()), AnchorId("a".into())), m);
            let v = spectral(&f, &k_feat, &levels, beta);
            let want = g(f64::from(m), f64::from(beta)) as f32;
            assert!((v - want).abs() < 1e-4, "m={m}: {v} vs {want}");
        }
    }

    #[test]
    fn spectral_is_exactly_splitting_invariant() {
        // Two feature-coincident cells (same source, two anchors wired
        // fully related via k_feat=1) carry the same operator S as one
        // merged cell, so splitting a mass across them leaves V unchanged
        // — the exact invariance DegreeQ only has for same-key coincidence.
        let levels = KSrcLevels { levels: [1.0; 5] };
        let s = SourceId("s".into());
        let (a1, a2) = (AnchorId("a1".into()), AnchorId("a2".into()));
        let mut k_feat = KFeatMatrix::default();
        k_feat.pairs.insert((a1.clone(), a2.clone()), 1.0);
        let beta = 3.0f32;

        let mut merged = Measure::new();
        merged.insert((s.clone(), a1.clone()), 6.0);
        let v_merged = spectral(&merged, &k_feat, &levels, beta);

        for &split in &[3.0f32, 1.0, 4.5] {
            let mut f = Measure::new();
            f.insert((s.clone(), a1.clone()), split);
            f.insert((s.clone(), a2.clone()), 6.0 - split);
            let v = spectral(&f, &k_feat, &levels, beta);
            assert!(
                (v - v_merged).abs() < 1e-4,
                "split={split}: {v} vs {v_merged}"
            );
        }
    }

    #[test]
    fn spectral_discounts_related_mass_with_the_correct_sign() {
        // Two equal cells, relatedness kappa via k_feat: V decreases as
        // kappa rises (concave g => Schur-concave in the spectrum).
        let levels = KSrcLevels { levels: [1.0; 5] };
        let s = SourceId("s".into());
        let (a1, a2) = (AnchorId("a1".into()), AnchorId("a2".into()));
        let beta = 5.0f32;
        let mut prev = f32::INFINITY;
        for &kappa in &[0.0f32, 0.3, 0.6, 0.9, 0.999] {
            let mut k_feat = KFeatMatrix::default();
            if kappa > 0.0 {
                k_feat.pairs.insert((a1.clone(), a2.clone()), kappa);
            }
            let mut f = Measure::new();
            f.insert((s.clone(), a1.clone()), 3.0);
            f.insert((s.clone(), a2.clone()), 3.0);
            let v = spectral(&f, &k_feat, &levels, beta);
            assert!(v < prev, "kappa={kappa}: {v} !< {prev}");
            prev = v;
        }
    }

    #[test]
    fn spectral_is_monotone_on_the_degreeq_harm_zone() {
        // DegreeQ's §1.2 failure: two mutually-unrelated atoms plus a
        // bridge atom related to both. Adding the bridge erodes DegreeQ's
        // score; the spectral value can only rise (a PSD addition lifts
        // every eigenvalue, by Weyl).
        let levels = KSrcLevels { levels: [1.0; 5] };
        let s = SourceId("s".into());
        let (a1, a2, bridge) = (
            AnchorId("a1".into()),
            AnchorId("a2".into()),
            AnchorId("bridge".into()),
        );
        // a1, a2 mutually unrelated (default 0); bridge fully related to both.
        let mut k_feat = KFeatMatrix::default();
        k_feat.pairs.insert((bridge.clone(), a1.clone()), 1.0);
        k_feat.pairs.insert((bridge.clone(), a2.clone()), 1.0);
        let beta = 1.0f32;

        let mut before = Measure::new();
        before.insert((s.clone(), a1.clone()), 0.02);
        before.insert((s.clone(), a2.clone()), 0.02);
        let v_before = spectral(&before, &k_feat, &levels, beta);

        let mut after = before.clone();
        after.insert((s.clone(), bridge.clone()), 0.5);
        let v_after = spectral(&after, &k_feat, &levels, beta);

        assert!(
            v_after >= v_before,
            "adding bridge lowered V: {v_after} < {v_before}"
        );
    }

    #[test]
    fn spectral_converges_under_fixed_spread_refinement() {
        // The headline fix: spread a fixed mass over N cells at fixed
        // pairwise relatedness (all k_feat = kappa). The Schatten form
        // diverges like N^{(1-p)/p}; the bounded-slope value converges.
        let levels = KSrcLevels { levels: [1.0; 5] };
        let kappa = 0.8f32;
        let beta = 1.0f32;
        let total_mass = 1.0f32;

        let v_of = |n: usize| -> f32 {
            let anchors: Vec<AnchorId> = (0..n).map(|i| AnchorId(format!("a{i}"))).collect();
            let mut k_feat = KFeatMatrix::default();
            for i in 0..n {
                for j in (i + 1)..n {
                    k_feat
                        .pairs
                        .insert((anchors[i].clone(), anchors[j].clone()), kappa);
                }
            }
            let mut f = Measure::new();
            for a in &anchors {
                f.insert((SourceId("s".into()), a.clone()), total_mass / n as f32);
            }
            spectral(&f, &k_feat, &levels, beta)
        };

        let (v2, v8, v16) = (v_of(2), v_of(8), v_of(16));
        // Bounded well below the divergent regime, and settling: the
        // analytic limit is g(kappa) + (1-kappa) ~= 0.788.
        assert!(v16 < 0.8, "not bounded: V(16)={v16}");
        assert!(v16 > v2, "should rise toward the limit: {v16} vs {v2}");
        assert!(
            (v16 - v8).abs() < 0.02,
            "not settling: V(8)={v8}, V(16)={v16}"
        );
    }

    #[test]
    fn spectral_contributions_sum_to_value_across_sign_classes() {
        // The Contribution breakdown must still sum to V, now spanning a
        // positive and a negative sign class (scored independently and
        // subtracted).
        let levels = KSrcLevels { levels: [1.0; 5] };
        let (a1, a2) = (AnchorId("a1".into()), AnchorId("a2".into()));
        let mut k_feat = KFeatMatrix::default();
        k_feat.pairs.insert((a1.clone(), a2.clone()), 0.5);
        let tax = Taxonomy::default();
        let beta = 2.0f32;

        let mut f = Measure::new();
        f.insert((SourceId("p1".into()), a1.clone()), 3.0);
        f.insert((SourceId("p2".into()), a2.clone()), 2.0);
        f.insert((SourceId("n1".into()), a1.clone()), -4.0);

        let (v, contribs) = value(
            &f,
            &k_feat,
            &tax,
            &mut SourceIndex::new(),
            &levels,
            0.0,
            Homogeneity::Spectral { beta },
        );
        let summed: f32 = contribs.iter().map(|c| c.contribution).sum();
        assert!((summed - v).abs() < 1e-5, "{summed} vs {v}");
        assert_eq!(contribs.len(), 3);
    }

    #[test]
    fn spectral_opposite_signs_do_not_crowd() {
        // Same anchor, maximally-related sources, but opposite signs: the
        // sign gate keeps them in separate operators, so each scores its
        // own g(|m|) and the mirrored pair nets to ~0.
        let levels = KSrcLevels { levels: [1.0; 5] };
        let k_feat = KFeatMatrix::default();
        let anchor = AnchorId("x".into());
        let beta = 2.0f32;
        let mut f = Measure::new();
        f.insert((SourceId("pos".into()), anchor.clone()), 2.0);
        f.insert((SourceId("neg".into()), anchor.clone()), -2.0);
        let v = spectral(&f, &k_feat, &levels, beta);
        assert!(v.abs() < 1e-4, "mirrored pair should net to 0, got {v}");
    }

    /// `value_scalar` must return exactly `value(...).0` for every
    /// homogeneity mode, including `Spectral` with a populated
    /// cross-anchor `k_feat` (the case that actually takes the cheaper
    /// `spectral_class_value`/`householder_ql_eigenvalues` path, unlike
    /// most other tests in this file which use an empty `k_feat`).
    #[test]
    fn value_scalar_matches_value_across_homogeneity_modes() {
        let mut rng = Xorshift(0xFACE_B00C_1357_2468);
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        let anchors: Vec<AnchorId> = (0..4).map(|i| AnchorId(format!("a{i}"))).collect();
        let modes = [
            Homogeneity::DegreeQ,
            Homogeneity::Degree1,
            Homogeneity::Spectral { beta: 1.5 },
        ];

        for trial in 0..8 {
            let n_sources = 3 + rng.next_range(10);
            let taxonomy = random_taxonomy(&mut rng, n_sources);
            let source_ids: Vec<SourceId> =
                (0..n_sources).map(|i| SourceId(format!("n{i}"))).collect();

            let mut f = Measure::new();
            for s in &source_ids {
                let n_cells = 1 + rng.next_range(2);
                let first = rng.next_range(anchors.len());
                for k in 0..n_cells {
                    let anchor = anchors[(first + k) % anchors.len()].clone();
                    let sign = if rng.next_range(2) == 0 { 1.0 } else { -1.0 };
                    let mass = sign * (rng.next_f32() + 0.05);
                    f.insert((s.clone(), anchor), mass);
                }
            }

            let mut k_feat = KFeatMatrix::default();
            for i in 0..anchors.len() {
                for j in (i + 1)..anchors.len() {
                    if rng.next_range(2) == 0 {
                        k_feat
                            .pairs
                            .insert((anchors[i].clone(), anchors[j].clone()), rng.next_f32());
                    }
                }
            }

            for &homogeneity in &modes {
                let mut idx_a = SourceIndex::new();
                let (v_full, _) = value(
                    &f,
                    &k_feat,
                    &taxonomy,
                    &mut idx_a,
                    &levels,
                    0.6,
                    homogeneity,
                );
                let mut idx_b = SourceIndex::new();
                let v_scalar = value_scalar(
                    &f,
                    &k_feat,
                    &taxonomy,
                    &mut idx_b,
                    &levels,
                    0.6,
                    homogeneity,
                );
                assert!(
                    (v_full - v_scalar).abs() < 1e-4 * (1.0 + v_full.abs()),
                    "trial={trial} homogeneity={homogeneity:?}: full={v_full} scalar={v_scalar}"
                );
            }
        }
    }
}
