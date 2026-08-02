//! Faithful re-embedding of `nrs-lib-ts` (`DAH_standards.ts`) impacts
//! and relations onto the 11-anchor palette in `anchors`. Each
//! function ports the old formula verbatim and emits already-computed
//! magnitudes as `TimeSupport::Moment` atoms — this is a *parity*
//! migration (preserve the old numbers as masses), not a redesign: the
//! old system's own duration handling already collapsed each impact to
//! a single scalar, so there is no raw day-count left to re-integrate
//! through the new engine's own time-support machinery. Letting the
//! new redundancy functional (crowding across atoms) take over from
//! here is what replaces the old ad hoc per-impact duration exponents.
//!
//! Source: `/home/ayaneso/dev/nrs-org/nrs-lib-ts/exts/DAH_standards.ts`.

use nrs_core::{AnchorId, Atom, RelKind, Relation, SourceId, TimeSupport, Transform};
use std::collections::BTreeMap;

use crate::anchors;

/// `mapClampThrow`: a linear remap, panicking if `x` falls outside
/// `[i_min, i_max]` (the old system threw in this case; import data
/// that trips this is bad data, not a recoverable case).
pub fn remap(x: f32, i_min: f32, i_max: f32, o_min: f32, o_max: f32) -> f32 {
    let factor = (x - i_min) / (i_max - i_min);
    assert!(
        (0.0..=1.0).contains(&factor),
        "value out of bounds: {x} not in [{i_min}, {i_max}]"
    );
    o_min + (o_max - o_min) * factor
}

/// `combinePow`'s unsigned branch (`src/utils.ts`): `(sum x_i^(1/w))^w`,
/// falling back to a plain max as `w -> 0`. Blend weights here are
/// always nonnegative, so the signed split the old code did around
/// this doesn't apply.
fn combine_pow(values: &[f32], weight: f32) -> f32 {
    if weight < 1e-4 {
        values.iter().cloned().fold(0.0f32, f32::max)
    } else {
        values
            .iter()
            .map(|x| x.powf(1.0 / weight))
            .sum::<f32>()
            .powf(weight)
    }
}

/// The old system's emotion group weight (`Emotion.subscoreWeight`,
/// `DAH_factors.ts`), used to normalise a base score split across
/// several named anchors (`#emotionVector`, `DAH_standards.ts`).
const EMOTION_SUBSCORE_WEIGHT: f32 = 0.6;

/// `#emotionVector`: split `base` across `blend` (anchor, weight)
/// pairs. One atom per anchor, magnitude already normalised — not our
/// `Atom::blend` mechanism (that's a plain proportional split; this
/// is the old power-mean-normalised one), so each emitted atom carries
/// a trivial single-anchor blend.
fn emotion_atoms(source: SourceId, base: f32, blend: &[(AnchorId, f32)]) -> Vec<Atom> {
    let weights: Vec<f32> = blend.iter().map(|(_, w)| *w).collect();
    let combined = combine_pow(&weights, EMOTION_SUBSCORE_WEIGHT);
    blend
        .iter()
        .map(|(anchor, w)| Atom {
            source: source.clone(),
            blend: vec![(anchor.clone(), 1.0)],
            magnitude: base * w.powf(0.9) / combined,
            time: TimeSupport::Moment,
        })
        .collect()
}

fn flat_atom(source: SourceId, anchor: AnchorId, magnitude: f32) -> Vec<Atom> {
    vec![Atom {
        source,
        blend: vec![(anchor, 1.0)],
        magnitude,
        time: TimeSupport::Moment,
    }]
}

// --- Moment impacts (no time decay) -----------------------------------

pub fn cry(source: SourceId, blend: &[(AnchorId, f32)]) -> Vec<Atom> {
    emotion_atoms(source, 4.0, blend)
}

pub fn nei(source: SourceId, factor: f32, sign: f32, blend: &[(AnchorId, f32)]) -> Vec<Atom> {
    let base = remap(factor.abs(), 0.0, 1.0, 0.0, 2.0) * sign;
    emotion_atoms(source, base, blend)
}

pub fn aei(source: SourceId, factor: f32, sign: f32, blend: &[(AnchorId, f32)]) -> Vec<Atom> {
    let base = remap(factor.abs(), 0.0, 1.0, 2.0, 3.0) * sign;
    emotion_atoms(source, base, blend)
}

/// "Legendary moment" markers, currently always routed to `comedy` as a
/// placeholder -- INFRA.md's migration mapping flags `epi`/`ehi` as
/// always needing individual review (`BuildExtras::needs_review`),
/// since the real emotion depends on the specific moment and can't be
/// determined from the tag alone.
pub fn ehi(source: SourceId) -> Vec<Atom> {
    emotion_atoms(source, 3.5, &[(anchors::comedy(), 1.0)])
}

pub fn epi(source: SourceId, factor: f32) -> Vec<Atom> {
    let base = remap(factor, 0.0, 1.0, 3.5, 4.5);
    emotion_atoms(source, base, &[(anchors::comedy(), 1.0)])
}

pub fn jumpscare(source: SourceId) -> Vec<Atom> {
    emotion_atoms(source, 1.0, &[(anchors::hooked(), 1.0)])
}

pub fn sleepless_night(source: SourceId) -> Vec<Atom> {
    emotion_atoms(source, 4.0, &[(anchors::hooked(), 1.0)])
}

pub fn politics(source: SourceId) -> Vec<Atom> {
    flat_atom(source, anchors::worldview(), 0.75)
}

pub fn interest_field(source: SourceId, new_field: bool) -> Vec<Atom> {
    flat_atom(
        source,
        anchors::gateway(),
        if new_field { 2.0 } else { 1.0 },
    )
}

pub fn additional(source: SourceId, value: f32) -> Vec<Atom> {
    flat_atom(source, anchors::additional(), value)
}

pub fn music(source: SourceId, music_base: f32) -> Vec<Atom> {
    flat_atom(source, anchors::music(), music_base * 3.0)
}

pub fn visual(source: SourceId, visual_type_factor: f32, base: f32, unique: f32) -> Vec<Atom> {
    let score = ((base * (unique + 2.0)) / 3.0) * visual_type_factor * 2.0;
    flat_atom(source, anchors::visual(), score)
}

pub fn writing(
    source: SourceId,
    character_complexity: f32,
    story_quality: f32,
    pacing: f32,
    originality: f32,
) -> Vec<Atom> {
    let m = 4.0;
    let base = m * (character_complexity + story_quality) / 2.0 * (1.0 + pacing) / 2.0
        * (1.0 + originality)
        / 2.0;
    flat_atom(source, anchors::writing(), base)
}

/// Splits cleanly into the two anchors the palette proposal already
/// names: `personal` (played/mastered this specific map) is the same
/// "invested in this one thing" feeling as `hooked`; `community` (how
/// big a deal it is in the osu scene) is the same cultural-currency
/// shape as `meme`. One `osuSong` tag, two atoms.
pub fn osu_song(source: SourceId, personal: f32, community: f32) -> Vec<Atom> {
    let personal_factor = remap(personal, 0.0, 1.0, 0.0, 0.5);
    let community_factor = remap(community, 0.0, 1.0, 0.0, 0.2);
    let mut atoms = flat_atom(source.clone(), anchors::hooked(), personal_factor);
    atoms.extend(flat_atom(source, anchors::meme(), community_factor));
    atoms
}

// --- Duration-decayed impacts (already-collapsed magnitudes) ---------

pub fn waifu(source: SourceId, days: f32) -> Vec<Atom> {
    let base = 1.2 * (days / 90.0).powf(0.35); // MP.factorWeight
    flat_atom(source, anchors::waifu(), base)
}

pub fn meme(source: SourceId, strength: f32, days: f32) -> Vec<Atom> {
    assert!(
        (0.0..2.0).contains(&strength),
        "strength out of [0, 2) range"
    );
    let base = strength * (days / 120.0).powf(0.4) * 4.0; // AP.factorWeight
    flat_atom(source, anchors::meme(), base)
}

pub fn pads(source: SourceId, days: f32, blend: &[(AnchorId, f32)]) -> Vec<Atom> {
    let (a, p) = (0.3, 1.3);
    let base = a * days.min(10.0).powf(p);
    emotion_atoms(source, base, blend)
}

/// `consumed`/`animeConsumed` -> Boredom. `duration_minutes` is the
/// total consumed time (`animeConsumed` computed this as
/// `episode_duration * episodes` before calling `consumed`).
pub fn consumed(source: SourceId, boredom: f32, duration_minutes: f32) -> Vec<Atom> {
    let (base_score, base_duration_minutes) = if duration_minutes < 10.0 {
        (0.1, 5.0)
    } else if duration_minutes < 120.0 {
        (0.3, 120.0)
    } else {
        (1.0, 20.0 * 12.0) // averageAnimeEpisodeDuration(20min) * 12
    };
    let ratio = duration_minutes / base_duration_minutes;
    let score = boredom * base_score * ratio.powf(0.05); // Boredom.factorWeight
    flat_atom(source, anchors::boredom(), score)
}

pub fn anime_consumed(
    source: SourceId,
    boredom: f32,
    episodes: f32,
    episode_duration_minutes: f32,
) -> Vec<Atom> {
    consumed(source, boredom, episode_duration_minutes * episodes)
}

pub fn dropped(source: SourceId) -> Vec<Atom> {
    flat_atom(source, anchors::boredom(), -0.5)
}

/// `musicConsumedProgress`: `consumedProgress` with `boredom` fixed at
/// `1.0` (`DAH_entry_progress.ts`).
pub fn music_consumed_progress(source: SourceId, duration_minutes: f32) -> Vec<Atom> {
    consumed(source, 1.0, duration_minutes)
}

// --- Relations ---------------------------------------------------------
// `gateOpen` (old: ScalarMatrix(0.0)) is a zero-effect relation in the
// old solve, and is dropped here rather than represented: an edge with
// weight 0 contributes nothing, so omitting it is exactly equivalent.

pub fn contains(from: nrs_core::EntryId, to: nrs_core::EntryId) -> Relation {
    contains_weighted(from, to, 1.0)
}

/// `<contains factor="...">`: same as `contains`, but with an
/// explicit fraction rather than the default full (`1.0`) borrow.
pub fn contains_weighted(from: nrs_core::EntryId, to: nrs_core::EntryId, factor: f32) -> Relation {
    Relation {
        kind: RelKind::Contains,
        from,
        to,
        transform: Transform::Scalar(factor),
    }
}

pub fn remix(from: nrs_core::EntryId, to: nrs_core::EntryId) -> Relation {
    Relation {
        kind: RelKind::Remix,
        from,
        to,
        transform: Transform::Scalar(0.2),
    }
}

pub fn feature_music(from: nrs_core::EntryId, to: nrs_core::EntryId) -> Relation {
    let mut map = BTreeMap::new();
    map.insert(anchors::music(), 0.2);
    Relation {
        kind: RelKind::FeatureMusic,
        from,
        to,
        transform: Transform::Regional(map),
    }
}

/// Structural discount across old box identities has no per-instance
/// content to disambiguate, so this uses the same box-level defaults
/// as everywhere else in the migration (`AP->comedy`, `MP->heartwarming`,
/// `CU->tragedy`) rather than trying to split it.
pub fn killed_by(
    from: nrs_core::EntryId,
    to: nrs_core::EntryId,
    potential: f32,
    effect: f32,
) -> Relation {
    let base = 0.4;
    let scale = potential * effect * base;
    let mut map = BTreeMap::new();
    for (anchor, coefficient) in [
        (anchors::comedy(), 0.2),
        (anchors::hate(), 0.1),
        (anchors::cozy(), 0.05),
        (anchors::tragedy(), 0.05),
        (anchors::heartwarming(), 0.2),
        (anchors::discomfort(), 0.1),
        (anchors::writing(), 0.1),
        (anchors::music(), 0.1),
        (anchors::boredom(), 0.1),
        // AV and Additional are 0 in the old DiagonalMatrix; the
        // sparse Regional map's default (0.0) already matches.
    ] {
        map.insert(anchor, coefficient * scale);
    }
    Relation {
        kind: RelKind::KilledBy,
        from,
        to,
        transform: Transform::Regional(map),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str) -> SourceId {
        SourceId(id.into())
    }
    fn e(id: &str) -> nrs_core::EntryId {
        nrs_core::EntryId(id.into())
    }

    fn only(atoms: Vec<Atom>) -> Atom {
        assert_eq!(atoms.len(), 1);
        atoms.into_iter().next().unwrap()
    }

    /// Clannad `A-MAL-2167`: `<writing character="0.5" story="0.5"
    /// pacing="0.8" originality="0.8" />`.
    #[test]
    fn writing_matches_clannad() {
        let atom = only(writing(s("A-MAL-2167"), 0.5, 0.5, 0.8, 0.8));
        assert_eq!(atom.blend, vec![(anchors::writing(), 1.0)]);
        // 4*(0.5+0.5)/2 * 1.8/2 * 1.8/2 = 4*0.5*0.9*0.9 = 1.62
        assert!((atom.magnitude - 1.62).abs() < 1e-4, "{}", atom.magnitude);
    }

    /// Clannad `A-MAL-2167`: `<visual type="animated" base="0.35"
    /// unique="0.75" />` (Animated's factor is 1.0).
    #[test]
    fn visual_matches_clannad() {
        let atom = only(visual(s("A-MAL-2167"), 1.0, 0.35, 0.75));
        assert_eq!(atom.blend, vec![(anchors::visual(), 1.0)]);
        assert!(
            (atom.magnitude - 0.641667).abs() < 1e-4,
            "{}",
            atom.magnitude
        );
    }

    /// Clannad `A-MAL-2167`: `<animeConsumedProgress boredom="0.8"
    /// episodes="23" />`, default 20-minute episodes.
    #[test]
    fn anime_consumed_matches_clannad() {
        let atom = only(anime_consumed(s("A-MAL-2167"), 0.8, 23.0, 20.0));
        assert_eq!(atom.blend, vec![(anchors::boredom(), 1.0)]);
        assert!(
            (atom.magnitude - 0.826451).abs() < 1e-4,
            "{}",
            atom.magnitude
        );
    }

    /// Clannad `F-VGMDB-1018`: `<nei base="0.3"
    /// emotions="CU-0.5:CP-0.5" />` on `A-MAL-2167`.
    #[test]
    fn nei_splits_across_two_anchors() {
        let atoms = nei(
            s("A-MAL-2167"),
            0.3,
            1.0,
            &[(anchors::tragedy(), 0.5), (anchors::cozy(), 0.5)],
        );
        assert_eq!(atoms.len(), 2);
        // Equal 0.5/0.5 split: combined = 2^-0.4, magnitude = 0.6/sqrt(2).
        let expected = 0.6 / std::f32::consts::SQRT_2;
        for atom in &atoms {
            assert!(
                (atom.magnitude - expected).abs() < 1e-4,
                "{}",
                atom.magnitude
            );
        }
    }

    /// A single full-weight anchor must reduce to the identity: no
    /// normalisation effect when there's nothing to split against.
    #[test]
    fn nei_single_anchor_is_identity() {
        let atom = only(nei(s("e"), 0.6, 1.0, &[(anchors::comedy(), 1.0)]));
        assert!((atom.magnitude - 1.2).abs() < 1e-5, "{}", atom.magnitude);
    }

    #[test]
    fn waifu_uses_its_own_weight_as_exponent() {
        let atom = only(waifu(s("e"), 90.0));
        // days == the 90-day reference point, so (days/90)^w == 1.
        assert!((atom.magnitude - 1.2).abs() < 1e-5, "{}", atom.magnitude);
    }

    #[test]
    fn contains_is_a_full_scalar_borrow() {
        let r = contains(e("album"), e("track"));
        assert_eq!(r.kind, RelKind::Contains);
        assert_eq!(r.transform, Transform::Scalar(1.0));
    }

    #[test]
    fn killed_by_scales_by_potential_and_effect() {
        let r = killed_by(e("a"), e("b"), 0.3, 1.0);
        match r.transform {
            Transform::Regional(map) => {
                let comedy = map.get(&anchors::comedy()).copied().unwrap();
                // 0.2 * (0.3 * 1.0 * 0.4) = 0.024
                assert!((comedy - 0.024).abs() < 1e-5, "{comedy}");
                assert!(
                    !map.contains_key(&anchors::visual()),
                    "visual must be absent (0 coefficient)"
                );
            }
            other => panic!("expected Regional, got {other:?}"),
        }
    }
}
