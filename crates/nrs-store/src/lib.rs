//! SQLite <-> `Snapshot` bridge, append-only event log, and CRUD
//! (INFRA.md §3, §5-§7): the `nrs-store` piece of the architecture
//! diagram. Depends only on `nrs-core`; the store never leaks `rusqlite`
//! types into it (INFRA.md §10: `stores -> core`, never the reverse).

mod crud;
mod error;
mod event;
mod events;
mod import;
mod preferences;
mod relkind;
mod resync;
mod review;
mod review_queue;
mod schema;
mod shared;
mod snapshot;
mod split;

use std::path::{Path, PathBuf};

use rusqlite::Connection;

pub use crud::EntryRelation;
pub use error::{Result, StoreError};
pub use event::EventKind;
pub use events::EventRecord;
pub use import::ImportExtras;
pub use preferences::Preference;
pub use resync::{ReboundEntry, ResyncPlan, RetitledEntry, SharedBase, SharedChange};
pub use review_queue::ReviewImpact;
pub use split::{
    flatten as flatten_split, infer as infer_split, remove_member as remove_split_member,
    validate as validate_split, SplitNode, SplitRule,
};
pub use shared::{
    expand as expand_shared_impact, member_impact_id, split_member_impact_id, ImpactMember,
    SharedImpact, MEMBER_ID_SEP,
};

use review::ReviewLog;

/// The `calibration` table is keyed by `medium` (INFRA.md open decision
/// #5); until that decision is made there is one curve under this key.
pub(crate) const DEFAULT_MEDIUM: &str = "default";

/// The `params` table's JSON: the knobs without a table of their own
/// (`K` lives in `kfeat`, calibration in `calibration`). Older blobs'
/// unknown fields are ignored.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredParams {
    #[serde(default)]
    pub time: nrs_core::TimeParams,
}

pub struct Store {
    conn: Connection,
    review: ReviewLog,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)?;
        let review = ReviewLog::load(Some(review_log_path(path)))?;
        Self::init(conn, review)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn, ReviewLog::load(None)?)
    }

    fn init(conn: Connection, review: ReviewLog) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::migrate(&conn)?;
        Ok(Store { conn, review })
    }
}

/// `nrs.db` -> `nrs.review.json`: a sibling of the database file, not a
/// table in it (see `review` module docs for why).
fn review_log_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("review.json")
}
