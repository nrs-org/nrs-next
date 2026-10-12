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
    kind      TEXT NOT NULL,            -- contains | credit | remix | feature_music | killed_by
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
    k REAL NOT NULL,                    -- pooling exponent in (0,1] (V6)
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

const SCHEMA_V2: &str = r#"
-- Retired before release. Keep the version slot so databases created by
-- development builds retain the same migration numbering.
SELECT 1;
"#;

const SCHEMA_V3: &str = r#"
-- The DAH_standards kind + natural parameters an impact/relation was
-- built from (JSON object of the form fields, e.g. {"factor":"0.5",
-- "sign":"1"}), so the entry page can show "nei factor=0.5" instead of
-- a raw anchor blend. NULL for anything predating this column (bulk
-- XML import, or a raw edit) -- those fall back to a raw display.
-- `relation.kind` already doubles as the DAH relation kind for the
-- four kinds DAH_standards covers, so only `dah_params` is needed
-- there; `impact` has no such column to reuse, hence `dah_kind` too.
ALTER TABLE impact ADD COLUMN dah_kind TEXT;
ALTER TABLE impact ADD COLUMN dah_params TEXT;
ALTER TABLE relation ADD COLUMN dah_params TEXT;
"#;

const SCHEMA_V4: &str = r#"
-- A `<contributor factor="..">` share: the fraction of the
-- DAH_standards formula's full-magnitude result this specific impact
-- actually carries (one `<impact>` tag with several contributors fans
-- out into one row per contributor at import time -- see
-- `nrs_import::snapshot::build_snapshot` -- each keeping the same
-- `dah_kind`/`dah_params` but its own share of the total). NULL means
-- unscaled (equivalent to 1.0); the kind editor multiplies the
-- formula's output by this before saving, so re-editing a split
-- impact no longer silently discards its share.
ALTER TABLE impact ADD COLUMN dah_factor REAL;
"#;

const SCHEMA_V5: &str = r#"
-- Provider aliases and deduplication are owned by type stores. Remove the
-- obsolete central table if an earlier development build created it.
DROP TABLE IF EXISTS entry_alias;
"#;

const SCHEMA_V6: &str = r#"
-- `kfeat` now holds pooling exponents, not redundancies (see
-- `nrs_core::KFeatMatrix`): the diagonal `(a, a, w)` is anchor a's own
-- exponent, an off-diagonal `(a, b, k)` the exponent at which a and b
-- pool together, and every unlisted entry defaults to 1 (additive).
-- Seed the 18-anchor palette and the legacy box/group structure: each
-- anchor's exponent is its old box's, anchors of one box pool at that
-- box's exponent, boxes of one group at the group's, everything else
-- adds. `INSERT OR IGNORE` keeps any row already set by hand.
INSERT OR IGNORE INTO anchor (id, label, family) VALUES
    ('additional', 'additional', 'extra'),
    ('boredom', 'boredom', 'extra'),
    ('comedy', 'comedy', 'feeling'),
    ('cozy', 'cozy', 'feeling'),
    ('discomfort', 'discomfort', 'feeling'),
    ('gateway', 'gateway', 'extra'),
    ('hate', 'hate', 'feeling'),
    ('heartwarming', 'heartwarming', 'feeling'),
    ('hooked', 'hooked', 'feeling'),
    ('hype', 'hype', 'feeling'),
    ('meme', 'meme', 'feeling'),
    ('music', 'music', 'craft'),
    ('tragedy', 'tragedy', 'feeling'),
    ('visual', 'visual', 'craft'),
    ('waifu', 'waifu', 'feeling'),
    ('wistful', 'wistful', 'feeling'),
    ('worldview', 'worldview', 'extra'),
    ('writing', 'writing', 'craft');
INSERT OR IGNORE INTO kfeat (a, b, k) VALUES
    ('boredom', 'boredom', 0.05),
    ('comedy', 'comedy', 0.4),
    ('cozy', 'cozy', 0.5),
    ('discomfort', 'discomfort', 0.35),
    ('hate', 'hate', 0.3),
    ('heartwarming', 'heartwarming', 0.35),
    ('hooked', 'hooked', 0.35),
    ('hype', 'hype', 0.4),
    ('meme', 'meme', 0.4),
    ('music', 'music', 0.3),
    ('tragedy', 'tragedy', 0.4),
    ('visual', 'visual', 0.1),
    ('waifu', 'waifu', 0.35),
    ('wistful', 'wistful', 0.4),
    ('writing', 'writing', 0.4),
    ('comedy', 'cozy', 0.6),
    ('comedy', 'discomfort', 0.6),
    ('comedy', 'hate', 0.6),
    ('comedy', 'heartwarming', 0.6),
    ('comedy', 'hooked', 0.6),
    ('comedy', 'hype', 0.4),
    ('comedy', 'meme', 0.4),
    ('comedy', 'tragedy', 0.6),
    ('comedy', 'waifu', 0.6),
    ('comedy', 'wistful', 0.6),
    ('cozy', 'discomfort', 0.6),
    ('cozy', 'hate', 0.6),
    ('cozy', 'heartwarming', 0.6),
    ('cozy', 'hooked', 0.6),
    ('cozy', 'hype', 0.6),
    ('cozy', 'meme', 0.6),
    ('cozy', 'tragedy', 0.6),
    ('cozy', 'waifu', 0.6),
    ('cozy', 'wistful', 0.6),
    ('discomfort', 'hate', 0.6),
    ('discomfort', 'heartwarming', 0.6),
    ('discomfort', 'hooked', 0.6),
    ('discomfort', 'hype', 0.6),
    ('discomfort', 'meme', 0.6),
    ('discomfort', 'tragedy', 0.6),
    ('discomfort', 'waifu', 0.6),
    ('discomfort', 'wistful', 0.6),
    ('hate', 'heartwarming', 0.6),
    ('hate', 'hooked', 0.6),
    ('hate', 'hype', 0.6),
    ('hate', 'meme', 0.6),
    ('hate', 'tragedy', 0.6),
    ('hate', 'waifu', 0.6),
    ('hate', 'wistful', 0.6),
    ('heartwarming', 'hooked', 0.35),
    ('heartwarming', 'hype', 0.6),
    ('heartwarming', 'meme', 0.6),
    ('heartwarming', 'tragedy', 0.6),
    ('heartwarming', 'waifu', 0.35),
    ('heartwarming', 'wistful', 0.6),
    ('hooked', 'hype', 0.6),
    ('hooked', 'meme', 0.6),
    ('hooked', 'tragedy', 0.6),
    ('hooked', 'waifu', 0.35),
    ('hooked', 'wistful', 0.6),
    ('hype', 'meme', 0.4),
    ('hype', 'tragedy', 0.6),
    ('hype', 'waifu', 0.6),
    ('hype', 'wistful', 0.6),
    ('meme', 'tragedy', 0.6),
    ('meme', 'waifu', 0.6),
    ('meme', 'wistful', 0.6),
    ('music', 'visual', 0.7),
    ('music', 'writing', 0.7),
    ('tragedy', 'waifu', 0.6),
    ('tragedy', 'wistful', 0.4),
    ('visual', 'writing', 0.7),
    ('waifu', 'wistful', 0.6);
"#;

const SCHEMA_V7: &str = r#"
-- Date the legacy dated impacts (meme, waifu, pads with one calendar
-- period in `dah_params`): their atoms get `Interval` time supports over
-- [period_from, period_to), in days since 1970-01-01, so scoring can
-- integrate them over time. Magnitudes are unchanged: an `Interval`
-- atom's magnitude is still the value of its whole period.
UPDATE impact_atom
SET time = json_object('Interval', json_object(
    'from', CAST(round(julianday(json_extract(i.dah_params, '$.period_from')) - 2440587.5) AS INTEGER),
    'to',   CAST(round(julianday(json_extract(i.dah_params, '$.period_to'))   - 2440587.5) AS INTEGER)))
FROM impact i
WHERE i.id = impact_atom.impact
  AND impact_atom.time = '"Moment"'
  AND julianday(json_extract(i.dah_params, '$.period_from')) IS NOT NULL
  AND julianday(json_extract(i.dah_params, '$.period_to')) IS NOT NULL;
INSERT INTO event (ts, kind, target, before, after, note)
SELECT CAST(strftime('%s', 'now') AS INTEGER), 'schema.migrate', NULL, NULL, NULL,
       'V7: ' || changes() || ' legacy dated atoms get Interval time supports'
WHERE changes() > 0;
"#;

const MIGRATIONS: &[&str] = &[
    SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7,
];

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let applied = user_version as usize;
    for (i, migration) in MIGRATIONS.iter().enumerate().skip(applied) {
        conn.execute_batch(migration)?;
        conn.pragma_update(None, "user_version", (i + 1) as i64)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_removes_the_retired_central_alias_table() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE entry_alias (entry TEXT, store TEXT, store_id TEXT);\
             PRAGMA user_version = 4;",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'entry_alias'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
    }

    #[test]
    fn migration_dates_single_period_impacts() {
        let conn = Connection::open_in_memory().unwrap();
        for migration in &MIGRATIONS[..6] {
            conn.execute_batch(migration).unwrap();
        }
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn.execute_batch(
            r#"INSERT INTO entry (id, store, store_id) VALUES ('e', 's', '1');
               INSERT INTO impact (id, target, created, dah_kind, dah_params)
                 VALUES ('dated', 'e', 0, 'meme', '{"days":"14","period_from":"2021-06-02","period_to":"2021-06-16"}'),
                        ('plain', 'e', 0, 'cry', '{}');
               INSERT INTO impact_atom (impact, source, blend, magnitude, time)
                 VALUES ('dated', 's', '[["meme",1.0]]', 1.0, '"Moment"'),
                        ('plain', 's', '[["cozy",1.0]]', 1.0, '"Moment"');"#,
        )
        .unwrap();
        migrate(&conn).unwrap();
        let time = |impact: &str| -> String {
            conn.query_row(
                "SELECT time FROM impact_atom WHERE impact = ?1",
                [impact],
                |row| row.get(0),
            )
            .unwrap()
        };
        // 2021-06-02 is day 18780 after 1970-01-01.
        assert_eq!(time("dated"), r#"{"Interval":{"from":18780,"to":18794}}"#);
        assert_eq!(time("plain"), r#""Moment""#);
    }

    #[test]
    fn fresh_database_seeds_the_legacy_exponents() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let k = |a: &str, b: &str| -> f64 {
            conn.query_row(
                "SELECT k FROM kfeat WHERE a = ?1 AND b = ?2",
                [a, b],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(k("waifu", "waifu"), 0.35);
        assert_eq!(k("heartwarming", "waifu"), 0.35);
        assert_eq!(k("comedy", "waifu"), 0.6);
        assert_eq!(k("music", "visual"), 0.7);
        let anchors: i64 = conn
            .query_row("SELECT COUNT(*) FROM anchor", [], |row| row.get(0))
            .unwrap();
        assert_eq!(anchors, 18);
    }
}
