//! Read access to the append-only `event` table (INFRA.md §6). No
//! undo/replay engine yet — just an honest read over what `crud.rs`
//! appended.

use crate::error::Result;
use crate::Store;

#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
    pub seq: i64,
    pub ts: i64,
    pub kind: String,
    pub target: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub note: Option<String>,
}

impl Store {
    pub fn event_log(&self) -> Result<Vec<EventRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, ts, kind, target, before, after, note FROM event ORDER BY seq")?;
        let rows = stmt.query_map([], |row| {
            Ok(EventRecord {
                seq: row.get(0)?,
                ts: row.get(1)?,
                kind: row.get(2)?,
                target: row.get(3)?,
                before: row.get(4)?,
                after: row.get(5)?,
                note: row.get(6)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}
