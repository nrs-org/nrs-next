//! The 1-10 score sent to the sites: nrs-impl's `DAH_anime_normalize`
//! (nrs/exts/DAH_anime_normalize.md), ported. Ten base anime
//! (`reference_anime.xml`, A-MAL-i scoring i on MAL's scale) are scored
//! with the database's own palette and params, and an entry's raw `V` is
//! interpolated between theirs, clamped to 1-10. They're rescored on
//! every sync, so the scale follows the functional as it changes, and
//! they never enter the database.

use std::collections::BTreeMap;

use nrs_core::{EntryId, Isotonic, Params, Snapshot};

const REFERENCE: &str = include_str!("reference_anime.xml");
const LEVELS: u8 = 10;

/// Raw `V`: the store's calibration curve (if any) left out.
fn raw(params: &Params) -> Params {
    Params {
        calibration: Isotonic::default(),
        ..params.clone()
    }
}

/// The 1-10 curve under `snapshot`'s palette and `params`; an error when
/// the reference document doesn't import cleanly, or its base anime
/// don't score in order (the scale would then not be monotone).
pub fn scale(snapshot: &Snapshot, params: &Params) -> Result<Isotonic, String> {
    let today = time::OffsetDateTime::now_utc().date();
    let document = nrs_import::xml::parse_str(REFERENCE, today).map_err(|e| e.to_string())?;
    if !document.warnings.is_empty() {
        return Err(format!("reference anime: {}", document.warnings.join("; ")));
    }
    let (mut reference, _, warnings) = nrs_import::build_snapshot(&document);
    if !warnings.is_empty() {
        return Err(format!("reference anime: {}", warnings.join("; ")));
    }
    reference.palette = snapshot.palette.clone();
    let values = nrs_core::score(&reference, &raw(params)).values;
    let mut knots = Vec::new();
    for level in 1..=LEVELS {
        let id = EntryId(format!("A-MAL-{level}"));
        let value = *values
            .get(&id)
            .ok_or_else(|| format!("reference anime {} didn't score", id.0))?;
        if let Some(&(previous, _)) = knots.last() {
            if value <= previous {
                return Err(format!(
                    "reference anime out of order: level {level} scores {value}, \
                     level {} {previous}",
                    level - 1
                ));
            }
        }
        knots.push((value, f32::from(level)));
    }
    Ok(Isotonic { knots })
}

/// Every entry's 1-10 score.
pub fn scores(snapshot: &Snapshot, params: &Params) -> Result<BTreeMap<EntryId, f32>, String> {
    let scale = scale(snapshot, params)?;
    let values = nrs_core::score(snapshot, &raw(params)).values;
    Ok(values
        .into_iter()
        .map(|(entry, value)| (entry, scale.apply(value)))
        .collect())
}

/// `score` (1-10) on a site's `points` scale: MAL's 10 rounds to whole
/// points, AniList's 100 to tenths, as nrs-impl did.
pub fn on_scale(score: f32, points: u32) -> u32 {
    (score * points as f32 / f32::from(LEVELS)).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_document_imports_cleanly() {
        let today = time::macros::date!(2026 - 10 - 16);
        let document = nrs_import::xml::parse_str(REFERENCE, today).unwrap();
        assert!(document.warnings.is_empty(), "{:?}", document.warnings);
        let (reference, _, warnings) = nrs_import::build_snapshot(&document);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reference.entries.len(), 14);
        assert_eq!(reference.relations.len(), 4, "A-MAL-10's featured music");
    }

    #[test]
    fn site_scales_round_like_nrs_impl() {
        assert_eq!(on_scale(7.46, 10), 7);
        assert_eq!(on_scale(7.46, 100), 75);
        assert_eq!(on_scale(1.0, 100), 10);
        assert_eq!(on_scale(10.0, 10), 10);
    }
}
