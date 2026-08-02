//! Offline parameter fitting (INFRA.md §8, roadmap phase 6). v1 fits a
//! single scalar -- `beta`, `Homogeneity::Spectral`'s saturation
//! constant -- from pairwise "A beats B" corrections
//! (`nrs_store::Preference`). Pure and I/O-free like `nrs-core` itself:
//! everything else (`q`, `k_src_levels`, `k_feat`, `relation_scaling`,
//! `calibration`) stays at its prior value -- INFRA.md's own "leaving
//! unidentified parameters at their prior", since a single
//! pairwise-preference signal can't identify a 5-number vector or a
//! whole matrix.

use nrs_core::{score, EntryId, Homogeneity, Params, Scores, Snapshot};
use nrs_store::Preference;

/// The result of one `fit` run.
#[derive(Debug, Clone)]
pub struct FitResult {
    pub beta: f32,
    /// Hinge loss at the chosen beta: 0 when every preference's winner
    /// already outscores its loser.
    pub loss: f32,
    /// How many preferences are still violated at the chosen beta.
    pub violations: usize,
    /// `prior` with `homogeneity` swapped to `Spectral { beta }`.
    pub params: Params,
}

const GRID_POINTS: usize = 61;
const LOG_SPAN: f32 = 3.0; // search center * 10^-3 ..= center * 10^3
const DEFAULT_CENTER: f32 = 1.0;

/// A bounded 1-D grid search over `beta` (INFRA.md §8: "a few hundred
/// `score()` calls in an optimiser loop" -- no general-purpose optimizer
/// needed for one scalar), minimizing a hinge loss over `preferences`.
/// Returns `prior` unchanged if there's nothing to fit against.
pub fn fit(snapshot: &Snapshot, prior: &Params, preferences: &[Preference]) -> FitResult {
    if preferences.is_empty() {
        return FitResult {
            beta: current_beta(prior).unwrap_or(DEFAULT_CENTER),
            loss: 0.0,
            violations: 0,
            params: prior.clone(),
        };
    }

    let center = natural_scale(snapshot, prior);

    let mut trial = prior.clone();
    let mut best_beta = center;
    let mut best_loss = f32::INFINITY;
    let mut best_violations = 0;

    for i in 0..GRID_POINTS {
        let t = i as f32 / (GRID_POINTS - 1) as f32; // 0..=1
        let log_factor = -LOG_SPAN + 2.0 * LOG_SPAN * t; // -LOG_SPAN..=LOG_SPAN
        let beta = center * 10f32.powf(log_factor);
        trial.homogeneity = Homogeneity::Spectral { beta };
        let scores = score(snapshot, &trial);
        let (loss, violations) = hinge_loss(&scores, preferences);

        let closer_to_center = (beta - center).abs() < (best_beta - center).abs();
        if loss < best_loss || (loss == best_loss && closer_to_center) {
            best_loss = loss;
            best_beta = beta;
            best_violations = violations;
        }
    }

    let mut params = prior.clone();
    params.homogeneity = Homogeneity::Spectral { beta: best_beta };
    FitResult {
        beta: best_beta,
        loss: best_loss,
        violations: best_violations,
        params,
    }
}

fn current_beta(params: &Params) -> Option<f32> {
    match params.homogeneity {
        Homogeneity::Spectral { beta } => Some(beta),
        _ => None,
    }
}

/// A rough order-of-magnitude reference for beta's search range: median
/// `|V|` under `prior`'s own params (whatever homogeneity that already
/// is). Redundancy discounting is sub-unity-multiplicative, not
/// something that shifts order of magnitude by many decades, so this is
/// a good-enough anchor for a wide log-spaced grid regardless of which
/// mode `prior` currently sits in.
fn natural_scale(snapshot: &Snapshot, prior: &Params) -> f32 {
    let scores = score(snapshot, prior);
    let mut magnitudes: Vec<f32> = scores
        .values
        .values()
        .map(|v| v.abs())
        .filter(|v| *v > 0.0)
        .collect();
    if magnitudes.is_empty() {
        return DEFAULT_CENTER;
    }
    magnitudes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = magnitudes.len() / 2;
    if magnitudes.len().is_multiple_of(2) {
        (magnitudes[mid - 1] + magnitudes[mid]) / 2.0
    } else {
        magnitudes[mid]
    }
}

/// `sum_{(winner,loser,conf)} conf * max(0, V[loser] - V[winner])`, plus
/// how many preferences are strictly violated (nonzero term). A
/// preference referencing an id outside `scores` scores 0 for that side.
fn hinge_loss(scores: &Scores, preferences: &[Preference]) -> (f32, usize) {
    let mut loss = 0.0;
    let mut violations = 0;
    for p in preferences {
        let winner_v = value_of(scores, &p.winner);
        let loser_v = value_of(scores, &p.loser);
        let margin = (loser_v - winner_v).max(0.0);
        if margin > 0.0 {
            violations += 1;
        }
        loss += p.confidence * margin;
    }
    (loss, violations)
}

fn value_of(scores: &Scores, entry: &EntryId) -> f32 {
    scores.values.get(entry).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrs_core::{
        Atom, Entry, Impact, ImpactId, Isotonic, KFeatMatrix, KSrcLevels, Palette, RelationScaling,
        SourceId, Taxonomy, TimeSupport,
    };

    fn tiny_snapshot() -> Snapshot {
        Snapshot {
            entries: vec![
                Entry {
                    id: EntryId("a".into()),
                    store: "s".into(),
                    store_id: "a".into(),
                    title_cache: None,
                },
                Entry {
                    id: EntryId("b".into()),
                    store: "s".into(),
                    store_id: "b".into(),
                    title_cache: None,
                },
            ],
            impacts: vec![
                Impact {
                    id: ImpactId("i1".into()),
                    target: EntryId("a".into()),
                    atoms: vec![Atom {
                        source: SourceId("a".into()),
                        blend: vec![(nrs_core::AnchorId("x".into()), 1.0)],
                        magnitude: 5.0,
                        time: TimeSupport::Moment,
                    }],
                },
                Impact {
                    id: ImpactId("i2".into()),
                    target: EntryId("b".into()),
                    atoms: vec![Atom {
                        source: SourceId("b".into()),
                        blend: vec![(nrs_core::AnchorId("x".into()), 1.0)],
                        magnitude: 1.0,
                        time: TimeSupport::Moment,
                    }],
                },
            ],
            relations: vec![],
            palette: Palette {
                anchors: Vec::new(),
                k_feat: KFeatMatrix::default(),
            },
            taxonomy: Taxonomy::default(),
        }
    }

    fn default_prior() -> Params {
        Params {
            q: 0.6,
            k_src_levels: KSrcLevels {
                levels: [0.95, 0.5, 0.3, 0.15, 0.0],
            },
            k_feat: KFeatMatrix::default(),
            homogeneity: Homogeneity::DegreeQ,
            calibration: Isotonic::default(),
            relation_scaling: RelationScaling::Fixed,
        }
    }

    fn pref(winner: &str, loser: &str) -> Preference {
        Preference {
            winner: EntryId(winner.into()),
            loser: EntryId(loser.into()),
            confidence: 1.0,
        }
    }

    #[test]
    fn no_preferences_returns_prior_unchanged() {
        let snap = tiny_snapshot();
        let prior = default_prior();
        let result = fit(&snap, &prior, &[]);
        assert_eq!(result.params, prior);
        assert_eq!(result.loss, 0.0);
        assert_eq!(result.violations, 0);
    }

    #[test]
    fn never_worse_than_the_natural_center_candidate() {
        // beta = center is always one of the grid points evaluated (the
        // search is symmetric in log-space around it), so fit()'s chosen
        // loss must never exceed the loss at that single candidate --
        // true by construction for any correct grid search that includes
        // its own center.
        let snap = tiny_snapshot();
        let prior = default_prior();
        let preferences = vec![pref("a", "b")];

        let center = natural_scale(&snap, &prior);
        let mut center_params = prior.clone();
        center_params.homogeneity = Homogeneity::Spectral { beta: center };
        let center_scores = score(&snap, &center_params);
        let (center_loss, _) = hinge_loss(&center_scores, &preferences);

        let result = fit(&snap, &prior, &preferences);
        assert!(
            result.loss <= center_loss + 1e-6,
            "fit loss {} exceeded center-candidate loss {}",
            result.loss,
            center_loss
        );
    }

    #[test]
    fn fitted_params_swap_homogeneity_to_spectral_within_search_bounds() {
        let snap = tiny_snapshot();
        let prior = default_prior(); // DegreeQ
        let preferences = vec![pref("a", "b")];
        let result = fit(&snap, &prior, &preferences);

        match result.params.homogeneity {
            Homogeneity::Spectral { beta } => assert_eq!(beta, result.beta),
            other => panic!("expected Spectral, got {other:?}"),
        }

        let center = natural_scale(&snap, &prior);
        let lo = center * 10f32.powf(-LOG_SPAN);
        let hi = center * 10f32.powf(LOG_SPAN);
        assert!(result.beta >= lo - 1e-6 && result.beta <= hi + 1e-6);

        // Every other knob stays at its prior value.
        assert_eq!(result.params.q, prior.q);
        assert_eq!(result.params.k_src_levels.levels, prior.k_src_levels.levels);
        assert_eq!(result.params.relation_scaling, prior.relation_scaling);
    }

    #[test]
    fn contradictory_preferences_do_not_panic() {
        let snap = tiny_snapshot();
        let prior = default_prior();
        let preferences = vec![pref("a", "b"), pref("b", "a")];
        let result = fit(&snap, &prior, &preferences);
        assert!(result.loss.is_finite());
        assert!(result.beta.is_finite() && result.beta > 0.0);
    }

    #[test]
    fn unknown_entry_ids_in_preferences_do_not_panic() {
        let snap = tiny_snapshot();
        let prior = default_prior();
        let preferences = vec![pref("ghost", "a")];
        let result = fit(&snap, &prior, &preferences);
        assert!(result.loss.is_finite());
    }
}
