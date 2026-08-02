//! The considered palette (`../../ANCHOR_PALETTE_PROPOSAL.md`), replacing
//! the legacy `nrs-lib-ts` 6-box valence/arousal grid + 3 craft factors
//! (`DAH_factors.ts`) this crate started as a literal migration target
//! of. Mined from the real dataset, not designed in the abstract -- see
//! the proposal doc for the evidence behind each split.

use nrs_core::AnchorId;

// -- feeling anchors, replacing the old AP/MP/CU boxes --------------

pub fn comedy() -> AnchorId {
    AnchorId("comedy".into())
}
pub fn hype() -> AnchorId {
    AnchorId("hype".into())
}
pub fn tragedy() -> AnchorId {
    AnchorId("tragedy".into())
}
pub fn wistful() -> AnchorId {
    AnchorId("wistful".into())
}
pub fn cozy() -> AnchorId {
    AnchorId("cozy".into())
}
pub fn heartwarming() -> AnchorId {
    AnchorId("heartwarming".into())
}
pub fn hate() -> AnchorId {
    AnchorId("hate".into())
}
pub fn discomfort() -> AnchorId {
    AnchorId("discomfort".into())
}

// -- previously-conflated special-case impacts, now their own anchors --

pub fn waifu() -> AnchorId {
    AnchorId("waifu".into())
}
pub fn meme() -> AnchorId {
    AnchorId("meme".into())
}
pub fn hooked() -> AnchorId {
    AnchorId("hooked".into())
}
pub fn worldview() -> AnchorId {
    AnchorId("worldview".into())
}
pub fn gateway() -> AnchorId {
    AnchorId("gateway".into())
}

// -- craft factors, kept (renamed from AL/AV/AM) ---------------------

pub fn writing() -> AnchorId {
    AnchorId("writing".into())
}
pub fn visual() -> AnchorId {
    AnchorId("visual".into())
}
pub fn music() -> AnchorId {
    AnchorId("music".into())
}

// -- scalar extras, kept --------------------------------------------

pub fn boredom() -> AnchorId {
    AnchorId("boredom".into())
}
pub fn additional() -> AnchorId {
    AnchorId("additional".into())
}
