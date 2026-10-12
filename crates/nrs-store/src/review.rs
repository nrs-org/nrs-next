//! Staging state for the anchor-palette migration review UI: the raw
//! `<!-- -->` comment captured at import time, and whether an impact's
//! anchor was a default guess rather than a confident classification.
//! Deliberately **not** a `nrs.db` schema addition -- this is
//! workflow/staging data, not graded content, so it lives in a JSON
//! sidecar file instead (`nrs.db` -> `nrs.review.json`). `impact.note`
//! (the *confirmed* value, once you've looked at the suggestion) is a
//! real column from `SCHEMA_V1`, untouched by any of this.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ReviewState {
    pub source_comment: Option<String>,
    pub needs_review: bool,
}

#[derive(Debug)]
pub(crate) struct ReviewLog {
    /// `None` for `Store::open_in_memory` -- no sidecar file, state
    /// lives only for the process lifetime.
    path: Option<PathBuf>,
    entries: BTreeMap<String, ReviewState>,
}

impl ReviewLog {
    pub(crate) fn load(path: Option<PathBuf>) -> Result<Self> {
        let entries = match &path {
            Some(p) if p.exists() => serde_json::from_str(&std::fs::read_to_string(p)?)?,
            _ => BTreeMap::new(),
        };
        Ok(ReviewLog { path, entries })
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        // Whole-file rewrite on every mutation: simple and correct at
        // this scale (thousands of entries at most, matching INFRA.md's
        // "naive is milliseconds" stance) -- no need for anything
        // fancier for staging data that isn't in the hot scoring path.
        let text = serde_json::to_string_pretty(&self.entries)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// One file write for however many impacts are being seeded at once
    /// (bulk import) -- there's no single-impact `seed` since nothing in
    /// this plan seeds outside of import (the review UI only reads via
    /// `impacts_for_entry`/`entries_needing_review` and mutates via
    /// `edit_impact`/`confirm_impact`, never adds a fresh suggestion).
    pub(crate) fn seed_bulk(
        &mut self,
        states: impl IntoIterator<Item = (String, ReviewState)>,
    ) -> Result<()> {
        self.entries.extend(states);
        self.save()
    }

    /// Drops the state of impacts that no longer exist (one file write).
    pub(crate) fn remove_bulk<'a>(
        &mut self,
        impact_ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        for id in impact_ids {
            self.entries.remove(id);
        }
        self.save()
    }

    pub(crate) fn get(&self, impact_id: &str) -> ReviewState {
        self.entries.get(impact_id).cloned().unwrap_or_default()
    }

    pub(crate) fn clear_needs_review(&mut self, impact_id: &str) -> Result<()> {
        self.entries
            .entry(impact_id.to_string())
            .or_default()
            .needs_review = false;
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_then_get_round_trips_in_memory() {
        let mut log = ReviewLog::load(None).unwrap();
        log.seed_bulk([(
            "i1".to_string(),
            ReviewState {
                source_comment: Some("comedy was weak ngl".into()),
                needs_review: true,
            },
        )])
        .unwrap();
        let state = log.get("i1");
        assert_eq!(state.source_comment.as_deref(), Some("comedy was weak ngl"));
        assert!(state.needs_review);
    }

    #[test]
    fn unseeded_impact_defaults_to_empty_state() {
        let log = ReviewLog::load(None).unwrap();
        assert_eq!(log.get("nonexistent"), ReviewState::default());
    }

    #[test]
    fn clear_needs_review_leaves_the_comment_untouched() {
        let mut log = ReviewLog::load(None).unwrap();
        log.seed_bulk([(
            "i1".to_string(),
            ReviewState {
                source_comment: Some("suicidal undertones".into()),
                needs_review: true,
            },
        )])
        .unwrap();
        log.clear_needs_review("i1").unwrap();
        let state = log.get("i1");
        assert!(!state.needs_review);
        assert_eq!(state.source_comment.as_deref(), Some("suicidal undertones"));
    }

    #[test]
    fn survives_a_reload_from_disk() {
        let dir =
            std::env::temp_dir().join(format!("nrs-store-review-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nrs.review.json");

        {
            let mut log = ReviewLog::load(Some(path.clone())).unwrap();
            log.seed_bulk([(
                "i1".to_string(),
                ReviewState {
                    source_comment: Some("comfy af".into()),
                    needs_review: true,
                },
            )])
            .unwrap();
        }
        {
            let log = ReviewLog::load(Some(path.clone())).unwrap();
            let state = log.get("i1");
            assert_eq!(state.source_comment.as_deref(), Some("comfy af"));
            assert!(state.needs_review);
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
