//! Re-embedding of the legacy `nrs-lib-ts` engine's impacts and
//! relations onto `nrs-core` types (INFRA.md §12 roadmap phase 3, notes
//! ch. "Migration by distillation"). `nrs.db` writing isn't built yet;
//! `xml::parse_file` + `snapshot::build_snapshot` together turn an
//! `nrsml` XML document into an in-memory `Snapshot`.

pub mod anchors;
pub mod legacy;
pub mod snapshot;
pub mod xml;

pub use legacy::*;
pub use snapshot::build_snapshot;
pub use xml::{parse_file, ImportError, ParsedDocument};
