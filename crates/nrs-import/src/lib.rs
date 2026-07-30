//! Re-embedding of the legacy `nrs-lib-ts` engine's impacts and
//! relations onto `nrs-core` types (INFRA.md §12 roadmap phase 3, notes
//! ch. "Migration by distillation"). XML parsing and `nrs.db` writing
//! aren't built yet — this crate currently holds only the pure
//! "old formula -> `Atom`/`Relation`" mapping layer.

pub mod anchors;
pub mod legacy;

pub use legacy::*;
