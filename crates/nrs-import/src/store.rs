//! The "-> nrs.db" half of roadmap phase 3 (INFRA.md §12): writes a
//! `Snapshot` built by [`crate::build_snapshot`] into an `nrs-store`
//! database. The in-memory `build_snapshot` path stays intact — this
//! writes on top of it, so XML export keeps working as a parity net
//! (INFRA.md §12 phase 3) independent of whether the result also gets
//! persisted.

use nrs_core::Snapshot;
use nrs_store::{Result, Store};

pub fn write_snapshot(store: &mut Store, snapshot: &Snapshot) -> Result<()> {
    store.import_snapshot(snapshot)
}
