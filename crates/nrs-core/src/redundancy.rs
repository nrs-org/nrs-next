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

use crate::linalg::jacobi_eigen;
use crate::measure::Measure;
use crate::types::{
    AnchorId, Contribution, Homogeneity, KFeatMatrix, KSrcLevels, SourceId, Taxonomy,
};

fn ancestor_chain(taxonomy: &Taxonomy, id: &SourceId) -> Vec<SourceId> {
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

/// `k_src`: 1.0 at an identical source; otherwise the level indexed by
/// the combined tree-distance to the deepest shared ancestor (notes
/// table: same character/scene, same work, same franchise, same
/// genre/vibe, unrelated), falling back to the "unrelated" level when
/// the two sources share no ancestor at all. The exact distance-to-level
/// mapping is a modelling choice pending `nrs-fit` (roadmap phase 6);
/// this only needs to be monotone in "how far apart".
pub(crate) fn k_src(a: &SourceId, b: &SourceId, taxonomy: &Taxonomy, levels: &KSrcLevels) -> f32 {
    if a == b {
        return 1.0;
    }
    let chain_a = ancestor_chain(taxonomy, a);
    let chain_b = ancestor_chain(taxonomy, b);
    for (da, node) in chain_a.iter().enumerate() {
        if let Some(db) = chain_b.iter().position(|n| n == node) {
            let dist = da + db;
            let idx = dist.saturating_sub(1).min(levels.levels.len() - 1);
            return levels.levels[idx];
        }
    }
    *levels.levels.last().unwrap()
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
    k_src_levels: &KSrcLevels,
    q: f32,
    homogeneity: Homogeneity,
) -> (f32, Vec<Contribution>) {
    // The spectral value is a whole different computation (it reads the
    // kernel's spectrum, not a per-cell crowding sum), so it branches out
    // before the additive path below rather than sharing its loop.
    if let Homogeneity::Spectral { beta } = homogeneity {
        return spectral_value(f, k_feat, taxonomy, k_src_levels, beta);
    }

    let cells: Vec<(&SourceId, &crate::types::AnchorId, f32)> = f
        .iter()
        .filter(|(_, m)| **m != 0.0)
        .map(|((s, a), m)| (s, a, *m))
        .collect();

    let mut total = 0.0;
    let mut contributions = Vec::with_capacity(cells.len());
    for &(source_i, anchor_i, mass_i) in &cells {
        // Crowding is a sum of nonnegative terms (no cancellation), but
        // accumulate it in f64: `RelationScaling::PowQ` at low q produces
        // masses down near f32's subnormal floor, and an f64 accumulator
        // keeps their sum out of the denormal range where precision
        // collapses.
        let mut crowding = 0.0f64;
        for &(source_j, anchor_j, mass_j) in &cells {
            if mass_i.signum() != mass_j.signum() {
                continue; // opposite valence: never mutually redundant
            }
            let k =
                k_feat.get(anchor_i, anchor_j) * k_src(source_i, source_j, taxonomy, k_src_levels);
            crowding += f64::from(k) * f64::from(mass_j.abs());
        }

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
    k_src_levels: &KSrcLevels,
    beta: f32,
) -> (f32, Vec<Contribution>) {
    let beta = f64::from(beta).max(f64::MIN_POSITIVE);
    let cells: Vec<(&SourceId, &AnchorId, f32)> = f
        .iter()
        .filter(|(_, m)| **m != 0.0)
        .map(|((s, a), m)| (s, a, *m))
        .collect();

    // Per-cell contribution, indexed like `cells`; filled from each sign
    // class's spectral attribution, then emitted in `cells` order so the
    // breakdown is deterministic and sums to the value.
    let mut contrib_by_cell = vec![0.0f64; cells.len()];
    for sign in [1.0f32, -1.0] {
        let idxs: Vec<usize> = (0..cells.len())
            .filter(|&i| cells[i].2.signum() == sign)
            .collect();
        let attrib = spectral_class(&cells, &idxs, k_feat, taxonomy, k_src_levels, beta);
        // Positive class adds its log-volume, negative class subtracts.
        for (local, &global) in idxs.iter().enumerate() {
            contrib_by_cell[global] = f64::from(sign) * attrib[local];
        }
    }

    let mut total = 0.0f64;
    let mut contributions = Vec::with_capacity(cells.len());
    for (i, &(source_i, anchor_i, mass_i)) in cells.iter().enumerate() {
        // Crowding is kept for the breakdown only (it does not drive the
        // spectral value): the same self-inclusive, same-signed crowding
        // `sum_j K_ij |m_j|` that DegreeQ reports, so `explain`'s columns
        // stay comparable across scoring modes.
        let mut crowding = 0.0f64;
        for &(source_j, anchor_j, mass_j) in &cells {
            if mass_i.signum() != mass_j.signum() {
                continue;
            }
            let k =
                k_feat.get(anchor_i, anchor_j) * k_src(source_i, source_j, taxonomy, k_src_levels);
            crowding += f64::from(k) * f64::from(mass_j.abs());
        }
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
    k_feat: &KFeatMatrix,
    taxonomy: &Taxonomy,
    k_src_levels: &KSrcLevels,
    beta: f64,
) -> Vec<f64> {
    let n = idxs.len();
    if n == 0 {
        return Vec::new();
    }

    // G_ab = sqrt(|m_a| |m_b|) * K(a, b), K = k_feat . k_src.
    let mut g = vec![vec![0.0f64; n]; n];
    for a in 0..n {
        let (s_a, an_a, m_a) = cells[idxs[a]];
        for b in a..n {
            let (s_b, an_b, m_b) = cells[idxs[b]];
            let k = f64::from(k_feat.get(an_a, an_b))
                * f64::from(k_src(s_a, s_b, taxonomy, k_src_levels));
            let g_ab = (f64::from(m_a.abs()) * f64::from(m_b.abs())).sqrt() * k;
            g[a][b] = g_ab;
            g[b][a] = g_ab;
        }
    }

    let (vals, vecs) = jacobi_eigen(g);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AnchorId;
    use std::collections::BTreeMap;

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

        let (_, contributions) = value(&f, &k_feat, &tax, &levels, 0.5, Homogeneity::DegreeQ);
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
            let (v, contributions) = value(&f, &k_feat, &tax, &levels, 0.1, Homogeneity::DegreeQ);
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
            let (v, _) = value(&f, &k_feat, &tax, &levels, q, Homogeneity::DegreeQ);
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
        assert_eq!(k_src(&s, &s, &tax, &levels), 1.0);
    }

    #[test]
    fn unrelated_sources_use_last_level() {
        let a = SourceId("a".into());
        let b = SourceId("b".into());
        let tax = Taxonomy::default();
        let levels = KSrcLevels {
            levels: [0.95, 0.5, 0.3, 0.15, 0.0],
        };
        assert_eq!(k_src(&a, &b, &tax, &levels), 0.0);
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
        let (v_same, _) = value(&same_cell, &k_feat, &tax, &levels, q, Homogeneity::DegreeQ);

        let mut distinct_cells: BTreeMap<_, _> = BTreeMap::new();
        distinct_cells.insert((SourceId("s1".into()), anchor.clone()), a);
        distinct_cells.insert((SourceId("s2".into()), anchor.clone()), a);
        let (v_distinct, _) = value(
            &distinct_cells,
            &k_feat,
            &tax,
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

        let (v_q, _) = value(&f, &k_feat, &tax, &levels, 1.0, Homogeneity::DegreeQ);
        let (v_1, _) = value(&f, &k_feat, &tax, &levels, 1.0, Homogeneity::Degree1);
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

        let (v, _) = value(&f, &k_feat, &tax, &levels, 0.5, Homogeneity::Degree1);
        let (v_scaled, _) = value(&scaled, &k_feat, &tax, &levels, 0.5, Homogeneity::Degree1);
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
            let (v, _) = value(&f, &k_feat, &tax, &levels, q, Homogeneity::Degree1);
            assert!((v - 5.0).abs() < 1e-5, "q={q}: {v}");
        }
    }

    // ---- Homogeneity::Spectral (redesign-tex spectral-bounded-slope) ----

    /// `g(t) = beta ln(1 + t/beta)`, the spectral gauge, for expectations.
    fn g(t: f64, beta: f64) -> f64 {
        beta * (1.0 + t / beta).ln()
    }

    fn spectral(f: &Measure, k_feat: &KFeatMatrix, levels: &KSrcLevels, beta: f32) -> f32 {
        let tax = Taxonomy::default();
        value(f, k_feat, &tax, levels, 0.0, Homogeneity::Spectral { beta }).0
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
}
