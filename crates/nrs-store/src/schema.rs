//! DDL for `nrs.db` (INFRA.md §5.1), applied idempotently via
//! `PRAGMA user_version` so a fresh DB and an already-migrated one both
//! just work.

use rusqlite::Connection;

use crate::error::Result;

const SCHEMA_V1: &str = r#"
CREATE TABLE entry (
    id          TEXT PRIMARY KEY,       -- ULID
    store       TEXT NOT NULL,          -- 'music' | 'anime' | ...
    store_id    TEXT NOT NULL,          -- id within that store
    title_cache TEXT,                   -- denormalised for display
    UNIQUE(store, store_id)
);

CREATE TABLE impact (
    id       TEXT PRIMARY KEY,
    target   TEXT NOT NULL REFERENCES entry(id),
    note     TEXT,                      -- the justification (P2)
    created  INTEGER NOT NULL
);
CREATE TABLE impact_atom (
    impact    TEXT NOT NULL REFERENCES impact(id),
    source    TEXT NOT NULL,            -- SourceId (an entry/character id)
    blend     TEXT NOT NULL,            -- JSON [[anchor,frac],...]
    magnitude REAL NOT NULL,            -- signed
    time      TEXT NOT NULL             -- JSON: Moment | Tail{peak,lambda,cutoff}
);
CREATE INDEX idx_impact_target ON impact(target);
CREATE INDEX idx_impact_atom_impact ON impact_atom(impact);

CREATE TABLE relation (
    id        TEXT PRIMARY KEY,
    kind      TEXT NOT NULL,            -- contains | remix | feature_music | killed_by
    from_e    TEXT NOT NULL REFERENCES entry(id),
    to_e      TEXT NOT NULL REFERENCES entry(id),
    transform TEXT NOT NULL             -- JSON: Scalar | Regional | Transport
);
CREATE INDEX idx_relation_from ON relation(from_e);
CREATE INDEX idx_relation_to ON relation(to_e);

CREATE TABLE anchor (
    id     TEXT PRIMARY KEY,            -- 'gut-punch', 'gorgeous-visuals', ...
    label  TEXT NOT NULL,
    family TEXT                          -- soft label only: emotion|craft|...
);
CREATE TABLE kfeat (                    -- sparse: only non-default pairs
    a TEXT NOT NULL REFERENCES anchor(id),
    b TEXT NOT NULL REFERENCES anchor(id),
    k REAL NOT NULL,                    -- redundancy in [0,1]
    PRIMARY KEY (a, b)
);

CREATE TABLE params (id INTEGER PRIMARY KEY CHECK(id = 1), json TEXT NOT NULL); -- Theta
CREATE TABLE calibration (medium TEXT PRIMARY KEY, isotonic TEXT NOT NULL);     -- g

CREATE TABLE event (                    -- the append-only history (§6)
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      INTEGER NOT NULL,
    kind    TEXT NOT NULL,              -- impact.add | relation.edit | param.set | ...
    target  TEXT,
    before  TEXT,                       -- JSON, nullable
    after   TEXT,                       -- JSON, nullable
    note    TEXT
);
"#;

const MIGRATIONS: &[&str] = &[SCHEMA_V1];

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let applied = user_version as usize;
    for (i, migration) in MIGRATIONS.iter().enumerate().skip(applied) {
        conn.execute_batch(migration)?;
        conn.pragma_update(None, "user_version", (i + 1) as i64)?;
    }
    Ok(())
}
