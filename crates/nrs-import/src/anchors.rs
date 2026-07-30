//! The starting palette: one anchor per legacy `nrs-lib-ts` factor
//! (`DAH_factors.ts`), named after the old short names so the mapping
//! is traceable at a glance. This is a literal 1:1 migration target,
//! not a considered palette — expected to be replaced/refined once
//! real anchors are chosen.

use nrs_core::AnchorId;

pub fn au() -> AnchorId {
    AnchorId("AU".into())
}
pub fn ap() -> AnchorId {
    AnchorId("AP".into())
}
pub fn mu() -> AnchorId {
    AnchorId("MU".into())
}
pub fn mp() -> AnchorId {
    AnchorId("MP".into())
}
pub fn cu() -> AnchorId {
    AnchorId("CU".into())
}
pub fn cp() -> AnchorId {
    AnchorId("CP".into())
}
pub fn al() -> AnchorId {
    AnchorId("AL".into())
}
pub fn av() -> AnchorId {
    AnchorId("AV".into())
}
pub fn am() -> AnchorId {
    AnchorId("AM".into())
}
pub fn boredom() -> AnchorId {
    AnchorId("Boredom".into())
}
pub fn additional() -> AnchorId {
    AnchorId("Additional".into())
}
