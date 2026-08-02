//! SQLite <-> `Snapshot` bridge, append-only event log, and CRUD
//! (INFRA.md §3, §5-§7): the `nrs-store` piece of the architecture
//! diagram. Depends only on `nrs-core`; the store never leaks `rusqlite`
//! types into it (INFRA.md §10: `stores -> core`, never the reverse).

mod crud;
mod error;
mod event;
mod events;
mod import;
mod params_blob;
mod relkind;
mod schema;
mod snapshot;

use std::path::Path;

use rusqlite::Connection;

pub use error::{Result, StoreError};
pub use event::EventKind;
pub use events::EventRecord;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::migrate(&conn)?;
        Ok(Store { conn })
    }
}
