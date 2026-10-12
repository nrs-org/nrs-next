//! The "-> nrs.db" half of roadmap phase 3 (INFRA.md §12): writes a
//! `Snapshot` built by [`crate::build_snapshot`] into an `nrs-store`
//! database. The in-memory `build_snapshot` path stays intact — this
//! writes on top of it, so XML export keeps working as a parity net
//! (INFRA.md §12 phase 3) independent of whether the result also gets
//! persisted.

use nrs_core::Snapshot;
use nrs_store::{ImportExtras, Result, Store};

use crate::snapshot::BuildExtras;

pub fn write_snapshot(store: &mut Store, snapshot: &Snapshot, extras: &BuildExtras) -> Result<()> {
    store.import_snapshot(snapshot, &import_extras(extras))
}

/// `BuildExtras` in the shape `nrs-store` persists.
pub fn import_extras(extras: &BuildExtras) -> ImportExtras {
    ImportExtras {
        comments: extras.source_comments.clone(),
        needs_review: extras.needs_review.clone(),
        credit_roles: extras.credit_roles.clone(),
        impact_dah: extras
            .impact_dah
            .iter()
            .map(|(id, (kind, params, factor))| {
                (id.clone(), (kind.to_string(), params.clone(), *factor))
            })
            .collect(),
    }
}
