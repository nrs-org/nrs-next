//! The `params.json` blob shape: only the knobs that don't already have a
//! dedicated table. `k_feat` lives in `kfeat` (set via `set_kfeat_pair`),
//! `calibration` in `calibration` (set via `set_calibration` — keyed by
//! `medium` in the schema per INFRA.md's open decision #5, one curve under
//! [`DEFAULT_MEDIUM`] until that decision is made).

use nrs_core::{Homogeneity, KSrcLevels, RelationScaling};
use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_MEDIUM: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ParamsBlob {
    pub q: f32,
    pub k_src_levels: KSrcLevels,
    pub homogeneity: Homogeneity,
    pub relation_scaling: RelationScaling,
}

impl Default for ParamsBlob {
    /// Matches `nrs-app`'s current hardcoded `params()` defaults.
    fn default() -> Self {
        ParamsBlob {
            q: 0.6,
            k_src_levels: KSrcLevels {
                levels: [0.95, 0.5, 0.3, 0.15, 0.0],
            },
            homogeneity: Homogeneity::DegreeQ,
            relation_scaling: RelationScaling::Fixed,
        }
    }
}
