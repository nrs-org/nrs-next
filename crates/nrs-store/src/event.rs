//! The append-only history mechanism (INFRA.md §6): every mutation appends
//! one `event` row inside the same transaction as the data write.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    EntryAdd,
    ImpactAdd,
    ImpactEdit,
    ImpactDelete,
    ImpactConfirm,
    RelationAdd,
    RelationEdit,
    RelationDelete,
    AnchorUpsert,
    KfeatSet,
    ParamSet,
    CalibrationSet,
    SnapshotImport,
    SnapshotResync,
    PreferenceRecord,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::EntryAdd => "entry.add",
            EventKind::ImpactAdd => "impact.add",
            EventKind::ImpactEdit => "impact.edit",
            EventKind::ImpactDelete => "impact.delete",
            EventKind::ImpactConfirm => "impact.confirm",
            EventKind::RelationAdd => "relation.add",
            EventKind::RelationEdit => "relation.edit",
            EventKind::RelationDelete => "relation.delete",
            EventKind::AnchorUpsert => "anchor.upsert",
            EventKind::KfeatSet => "kfeat.set",
            EventKind::ParamSet => "param.set",
            EventKind::CalibrationSet => "calibration.set",
            EventKind::SnapshotImport => "snapshot.import",
            EventKind::SnapshotResync => "snapshot.resync",
            EventKind::PreferenceRecord => "preference.record",
        }
    }
}
