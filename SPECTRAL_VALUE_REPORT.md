# Spectral packing value: where we are, and what to rethink

Status report, 2026-10-09. Prototype: `scratch/theta-value/` (Python, analysis only; never writes `nrs.db`).

## 1. Goal and constraints

We need a redundancy functional V(m; K) that turns an entry's resolved measure into a score. It has to satisfy:

| Axiom | Meaning |
|---|---|
| A1 | K = I (nothing redundant): V = Σ m |
| A2 | no bonus: V ≤ Σ m |
| A3 | homogeneous: V(λm) = λV(m) |
| A4 | monotone in mass |
| A5 | monotone in similarity: larger K, smaller V |
| A6 | blocks with zero cross-similarity add |
| A8 | peak preference: on an equicorrelated cluster, concentrating mass scores higher (strict Schur-convexity) |
| A9 | a fixed total mass spread over a growing correlated cluster → 0 |
| A10 | n unit atoms on a correlated cluster → ∞ |

A9 and A10 together force unbounded but sublinear growth. Earlier candidates each failed one of them: threshold-theta fails A9, and Choquet-DNN fails A10 (it saturates at 1/κ).

The calibration targets come from the old nrs-lib-ts engine, replayed exactly on the migrated db (`old_engine.py`):

- the old anime top 4 order (Ave Mujica, Clannad AS, Re:Stage, Nijigaku);
- Kokuhaku Jikkou Iinkai as the #1 franchise, by a large margin (×1.34 in the replay).

## 2. The functional: V_w

    V_w(m; K) = max { Σᵢ mᵢ sᵢ^(1−w) : s ≥ 0, λ_max(D_s^½ K D_s^½) ≤ 1 },   0 < w < 1

Signed measures score as V_w(m⁺) − V_w(m⁻). K must be PSD, entrywise in [0, 1], with unit diagonal.

- **Origin.** This is the external "Spectral Packing Value" proposal (w = ½), generalized to any w. The constraint is an LMI that doesn't depend on m or w, and the objective is concave, so this is a single convex program.
- **Axioms.** A1–A6 and A8–A10 all hold. They are tested in `tests/test_spectral.py` at several w values.
- **Closed forms:**
  - n atoms of mass a at similarity κ give a·n / (1 + (n−1)κ)^(1−w) ~ a·κ^(w−1)·n^w;
  - K = 11ᵀ gives ‖m‖_{1/w}, the old engine's per-factor pooling;
  - K = I gives Σ m.
- **Bounds.** w is a universal floor: for any admissible K, a·n^w ≤ V_w(a·1; K) ≤ a·n. So K can only make growth faster than n^w, never slower. On an equicorrelated cluster the local exponent is w + (1−w)/(1+(n−1)κ). It starts near 1 and falls to w at around n ≈ 1/κ.
- **Solver.** It uses a rank-one Lagrangian dual, which is convex, smooth, and needs neither K⁻¹ nor K^½:

      V_w = min_y  yᵀKy + c Σ mᵢ^(1/w) (Ky)ᵢ^(−r),   r = 2(1−w)/w,  c = w(1−w)^((1−w)/w)

  Rank one loses nothing: on a connected component of K > 0 with positive masses, the Perron eigenvalue is simple. Damped Newton solves it, with Jacobi scaling and pure Newton steps near the optimum. The primal point sᵢ ∝ ((1−w)mᵢ/(Ky)ᵢ²)^(1/w) gives a certified lower bound. Brackets sit at roundoff (≤ 2e-12) on real data for w ∈ [0.1, 0.9].
- **Speed.** All 413 entries (229 anime and 184 franchises/artists) take about 0.8 s of CPU. The proposal's five bundle fixtures reproduce to 10 digits, with brackets of 1e-16 instead of the proposal's 4e-3 (SLSQP).

**Verdict: the functional is not the problem.** Every difficulty below is about *what we feed it*.

## 3. The kernel as it stands

    K = s · (F ∘ T) + (1 − s) · I

| Part | Meaning | Current values |
|---|---|---|
| s | ceiling: the similarity of distinct points on the same anchor at the same time | 0.75 |
| F | anchor kernel: F(a,a) = 1, plus legacy factor-box and subscore-group levels 2^(1−w_old) − 1 | AP/CU 0.516, MP 0.569, Emotion 0.320, Art 0.231, else 0 |
| T | time: λ + (1−λ)·M. M is 1 within an impact, the RBF overlap of two dated periods, else 0 | λ 0.4–1, ℓ 30 days |

Every factor is PSD, so K is PSD by construction. Decisions taken along the way:

- **k_src is dropped,** and source no longer enters K.
- **No merging.** Distinct points are never identified, and K_ij ≤ s < 1.
- **k_feat stays below 1 for the same anchor** (the ceiling s plays the role of the old subscore weight).
- **Feature × time is a product** (an AND), not a sum.

## 4. Results so far

Anime ranks are given in the order AM CL RS NJ (exact = `1 2 3 4`). "KJI" is Kokuhaku's franchise rank and its lead over #2.

| Setup | w | s | λ | Anime | KJI |
|---|---|---|---|---|---|
| cell-level borrowing | 0.6 | 0.75 | 0.7 | exact | #1 ×1.25 |
| cell-level borrowing | 0.66 | 0.75 | 1 | exact | #1 ×1.28 |
| impact-level borrowing (dump hack) | 0.6 | 0.75 | 0.7 | exact | #1 ×1.39 |
| impact-level borrowing | 0.66 | 0.75 | 1 | exact | #1 ×1.42 |
| impact-level borrowing | 0.35 | 0.75 | 0.7 | 1 2 4 3 | #1 ×1.26 |
| impact-level borrowing | 0.35 | 0.5 | 0.4 | exact | #1 ×1.29 |

- Raising w raises Kokuhaku's lead, but past roughly 0.66 Clannad and Re:Stage swap.
- λ trades Clannad against Re:Stage: higher λ helps Clannad.
- s mostly rescales values; it changes ranks very little.
- Clannad vs Re:Stage is often within 0.02–0.2, so treat exact-order claims as fragile.

## 5. What we learned

### 5.1 With k_src gone, the time kernel carries all within-anchor docking

Scoring the engine's cell measure with only k_feat (and no time structure) merges everything on one anchor into a single point. 21k of 23.7k points collapse, and mass on an anchor adds linearly. The ranking then flips (Re:Stage #1). Whatever separates repeated impacts — impact identity or time — is what produces n^w docking at all.

### 5.2 Dated impacts are rare

Only legacy meme, waifu and pads impacts have a calendar period: 607 impacts, almost all memes. The period lives in `impact.dah_params` and is kept only when there was a single range. Every atom is `Moment`, and `impact.created` is the migration time. Dated mass is about 5% of the total, so ℓ barely moves rankings. Even treating every dated impact as simultaneous changes the most-dated entries by ≤ 3%. For the other ~95% of mass, λ effectively *is* the time kernel.

### 5.3 Borrowing granularity matters a lot

The engine's relation solve returns mass per (source, anchor). Borrowed mass therefore arrived with impacts pre-summed, so five memes on a song counted linearly when borrowed but docked when owned. The dump hack tags each atom's source with its impact before the solve (`THETA_DUMP_IMPACTS=1`). The solve is linear and never mixes sources, so it yields the exact impact-level measure; totals match to 6e-8. That changed franchise values substantially: Re:STAGE! went from 66.6 to 55.1, and Kokuhaku's lead from ×1.25 to ×1.39.

### 5.4 We have been preprocessing the measure, and that is the design flaw

| What the prototype does | What it really is |
|---|---|
| `points()`: own mass per impact, borrowed mass per cell | two measure spaces mixed together |
| `source|impact` tagging in the dump | an "event" coordinate, added outside the engine |
| the proposed Python time slicing | a time coordinate, added outside the engine |
| legacy duration formulas (`days^0.35`, …) in the importer | time docking baked into mass, because the measure has no time axis |
| `merge` (removed) | papering over a measure space that is too coarse |

**Principle:** the engine emits a measure on the space K needs, and V scores it without preprocessing.

### 5.5 Splitting invariance: what holds, and what can't

- Chopping a time support, or splitting mass within one impact on the same anchor: invariant.
- Recording one event as two impacts: **not** invariant. It can't be, unless identical atoms add exactly, and then (with no-bonus and monotonicity) nothing docks at all. The old engine had the same property: each impact was a separate term in its ℓ_{1/w} pool.
- So **the impact (event) is the unit.**

### 5.6 Real integration *does* work, if the integrated quantity is embedded mass

An earlier claim said that refining a time density into ever-finer atoms sends V → 0, so a fixed grid Δ would have to be a model constant. That claim was **wrong**: it refined the value-mass m.

The quantity that integrates is the old engine's embedded mass e = m^(1/w). Split an atom into n coincident pieces of embedded mass e/n, each with value-mass (e/n)^w. At κ = 1, V gives ‖((e/n)^w, …)‖_{1/w} = e^w = m, exactly, for any n. Coincident atoms must stay *unmerged* for this to work.

Numerically, a uniform embedded density of 1/day with w = 0.35 and ℓ = 30, cut into n pieces:

| | n=1 | 4 | 16 | 64 | 256 | 1024 |
|---|---|---|---|---|---|---|
| λ=1, D=365 | 7.88501 | 7.88501 | 7.88501 | 7.88501 | 7.88501 | 7.88501 |
| λ=0.7, D=30 | 3.28846 | 3.33586 | 3.33869 | 3.33886 | 3.33887 | 3.33888 |
| λ=0.7, D=365 | 7.88501 | 9.29727 | 9.44359 | 9.44542 | 9.44553 | 9.44554 |

What follows:

- **Resolution is a numerical tolerance, not a model constant.** About 64 pieces is enough.
- **λ = 1 reproduces the old duration law exactly:** V = (εD)^w. That law is "embedded mass grows linearly in time" (`REDESIGN.md` §158 already says this).
- **λ < 1 adds long-range decorrelation.** At w = 0.35 and λ = 0.7, the effective exponent over 30–365 days is about 0.42, close to the old meme exponent of 0.4. So one global w plus K covers both the old waifu and meme laws.
- **A9 is not violated.** It holds total *m* fixed; refinement holds total *e* fixed.

### 5.7 w is universal; per-kind behavior lives in K

There is no per-anchor w. Anchors are a detail of K, not of V. Different decay rates for different kinds come from how alike the moments of one phase are, which is K's job. The floor bound in §2 means w must be the slowest growth anything should ever have. The old exponents range from 0.35 (MP/waifu) to 0.4 (AP/meme), with the group exponents 0.6 and 0.7. So w ≈ 0.35 is the natural floor, but the ranking sweeps preferred about 0.6 *with the current K*. Those two need reconciling once the measure is fixed.

### 5.8 PADS is probably a bug

The old PADS formula, 0.3·min(days, 10)^1.3, is superlinear in duration. No V can produce that (no-bonus caps growth at linear), and nothing documents why. Treat it as a bug, or as duration standing in for intensity.

## 6. Proposed design

**Measure space.** X = anchor × event. Source is not a coordinate, for now.

- A dated impact's event is a time density over its period. One candidate shape is a half-Gaussian peaking at `from` with σ derived from `to − from`. Its kernel between two impacts has a closed form: the §5 full-Gaussian formula times a bivariate-normal CDF.
- An undated Moment is one event at an unknown time, keyed by its impact.

**Mass units.**
- The engine integrates *embedded* mass e linearly over time support (P6 splittability) and through relations.
- V converts each piece to value-mass e^w.
- A Moment atom's entered value m corresponds to e = m^(1/w).

**Kernel.** The private part attaches to the impact, not to every atom:

    K = s · (F ∘ T) + (1 − s) · ([same impact] ∘ T)

- Pieces of one impact at the same moment have similarity 1, so they recombine in e-space and refinement converges.
- Two different impacts are at most s alike.
- Both terms are PSD.

**Scoring.** V_w(atoms of μ, K), evaluated with quadrature over each density. No preprocessing.

**Engine changes.**
- `build_measures` and the relation solve are keyed by (anchor, event) instead of (source, anchor).
- Time supports become densities in e-units.
- The legacy duration formulas in `nrs-import/src/legacy.rs` (waifu, meme, pads) become plain intensities over the period.

## 7. Open questions

1. **Borrowing units.** Relation weights would scale *embedded* mass, which is what the old engine did. In value units that means a weight of 0.2 acts like 0.2^w (≈ 0.57 at w = 0.35): much stronger borrowing than the current linear 0.2. This likely moves franchise rankings more than anything else here. Keep it, or compensate the weights?
2. **w is now part of the measure convention**, because e = m^(1/w). Is one universal w acceptable as a stored-data convention, given that changing w re-interprets stored intensities?
3. **How are magnitudes entered?** Value units for Moments, and embedded intensity per day for periods? Or one unit for everything?
4. **The per-impact ceiling s.** Its value, and recalibrating F: the current levels were derived for Choquet-DNN.
5. **λ for unknown relative time.** Keep it as a free parameter, or derive it as the time kernel averaged over a prior on when an undated impact happened?
6. **Undated impacts as separate events.** This is what makes repeated cry/aei/nei on one anchor dock, as in the old engine. Is that the intended semantics?
7. **Signed mass.** Use e = |m|^(1/w) per sign class, scored separately as now?
8. **Rankings must be re-run** under the new units. Every number in §4 used value-unit masses with legacy duration formulas.

## 8. Files

| Path | What |
|---|---|
| `scratch/theta-value/nrs_theta/spectral.py` | V_w solver: rank-one dual Newton, certified bracket |
| `scratch/theta-value/nrs_theta/time_kernel.py` | interval RBF kernel-mean time kernel |
| `scratch/theta-value/rank_spectral.py` | ranking and sweeps: `--s --w --lam --ell --measure` |
| `scratch/theta-value/tests/test_spectral.py`, `test_time_kernel.py` | axioms, closed forms, SDP agreement, brackets |
| `scratch/theta-value/dump/src/main.rs` | `THETA_DUMP_IMPACTS=1` impact-tagged dump (the hack from §5.3) |
| `scratch/theta-value/data/{anime,mega}-impacts.json` | impact-level dumps |
| `scratch/theta-value/old_engine.py` | exact replay of the old engine (calibration targets) |
| `scratch/theta-value/README.md` | running log, with full sweep tables |
| `~/Downloads/Spectral_Packing_Value_Proposal.md` | the original w = ½ proposal |
