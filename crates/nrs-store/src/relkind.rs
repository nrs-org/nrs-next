//! `RelKind` <-> the `relation.kind` TEXT column (INFRA.md §5.1).

use nrs_core::RelKind;

use crate::error::StoreError;

pub(crate) fn rel_kind_to_str(kind: RelKind) -> &'static str {
    match kind {
        RelKind::Contains => "contains",
        RelKind::Credit => "credit",
        RelKind::Remix => "remix",
        RelKind::FeatureMusic => "feature_music",
        RelKind::KilledBy => "killed_by",
    }
}

pub(crate) fn rel_kind_from_str(s: &str) -> Result<RelKind, StoreError> {
    match s {
        "contains" => Ok(RelKind::Contains),
        "credit" => Ok(RelKind::Credit),
        "remix" => Ok(RelKind::Remix),
        "feature_music" => Ok(RelKind::FeatureMusic),
        "killed_by" => Ok(RelKind::KilledBy),
        other => Err(StoreError::InvalidData(format!(
            "unknown relation kind: {other}"
        ))),
    }
}
