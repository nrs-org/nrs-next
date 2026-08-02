//! Re-embedding of the legacy `nrs-lib-ts` engine's impacts and
//! relations onto `nrs-core` types (INFRA.md §12 roadmap phase 3, notes
//! ch. "Migration by distillation"). `xml::parse_file` +
//! `snapshot::build_snapshot` turn an `nrsml` XML document into an
//! in-memory `Snapshot`; `store::write_snapshot` persists it to `nrs.db`.

pub mod anchors;
pub mod legacy;
pub mod roles;
pub mod script;
pub mod snapshot;
pub mod store;
pub mod xml;
pub mod xtree;

pub use legacy::*;
pub use snapshot::{build_snapshot, BuildExtras};
pub use store::write_snapshot;
pub use xml::{parse_file, ImportError, ParsedDocument};
