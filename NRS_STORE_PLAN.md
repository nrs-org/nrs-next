# Plan: build `nrs-store` (INFRA.md §5–§7, roadmap phase 3–4 bridge)

> Handoff plan for implementing the SQLite persistence layer. Everything downstream
> (finishing XML→`nrs.db`, wiring `musiclib-rs`, CRUD in the app, `nrs-fit`'s event
> log) is blocked on this. Depends only on `nrs-core`, which is done.

## Goal

`nrs-store` = the SQLite ⇄ `Snapshot` bridge + append-only event log + CRUD
(INFRA.md §3 diagram: "load Snapshot, write events, CRUD (SQLite: nrs.db)").
It owns `nrs.db`. It must **not** leak SQL types into `nrs-core` — the dependency
arrow is `store → core`, never the reverse (INFRA.md §10).

## Decisions already made (don't re-litigate)

- **`rusqlite` with the `bundled` feature**, raw SQL, no ORM. INFRA.md §11 explicitly
  rejects an ORM ("raw SQL against SQLite is fine at this scale"); `musiclib-rs`
  already depends on `rusqlite 0.32` bundled, so match that version.
- **Blobs are JSON in TEXT columns** (INFRA.md §5.1, §11: "human-inspectable;
  interpretability-first"). That means `blend`, `time`, `transform`, `params`,
  `calibration` serialize via serde_json.
- **Add an optional `serde` feature to `nrs-core`** rather than duplicating the type
  definitions in the store. Default-off so the pure/WASM build stays lean; `nrs-store`
  turns it on. (`nrs-app` deliberately kept serde out of core for one query-param enum —
  that stays; this is a separate, gated derive path.)

## Step 0 — housekeeping

- Delete the stray `crates/nrs-next/crates/nrs-import/src/snapshot.rs` tree — it's an
  accidental nested copy, not a workspace member.

## Step 1 — `serde` feature on `nrs-core`

In `nrs-core/Cargo.toml`: `serde = { version = "1", features = ["derive"], optional = true }`
and `[features] serde = ["dep:serde"]`.

Add `#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]`
to the types that get persisted as JSON blobs (and their transitive members):
`AnchorId`/`SourceId`/`EntryId`/`ImpactId` (via the `id_type!` macro), `TimeSupport`,
`Transform`, `KFeatMatrix`, `KSrcLevels`, `Homogeneity`, `RelationScaling`, `Isotonic`,
`Params`. Keep it purely additive — no behavior change when the feature is off.
Confirm `cargo build -p nrs-core` (no features) is unchanged and
`cargo build -p nrs-core --features serde` compiles.

## Step 2 — scaffold the crate

- `crates/nrs-store/`, add to workspace `members`.
- deps: `nrs-core = { path = "../nrs-core", features = ["serde"] }`, `rusqlite`
  (bundled), `serde`, `serde_json`, `thiserror`, `time`, `ulid` (for the ULID ids the
  schema uses).
- `pub struct Store { conn: rusqlite::Connection }` with `Store::open(path)` /
  `Store::open_in_memory()` for tests. On open: `PRAGMA foreign_keys = ON`, then run
  migrations.
- A `StoreError` enum (`thiserror`) wrapping `rusqlite::Error` and
  `serde_json::Error`.

## Step 3 — schema + migrations

Embed the schema DDL from INFRA.md §5.1 verbatim (tables: `entry`, `impact`,
`impact_atom`, `relation`, `anchor`, `kfeat`, `params`, `calibration`, `event`).
Apply idempotently: a tiny versioned runner keyed on `PRAGMA user_version` (start at 1;
migrations are an ordered list so phase-4+ columns can be added later). Keep the DDL as
a `.sql` string constant in the crate so it's diffable.

## Step 4 — `load_snapshot`

`fn load_snapshot(&self) -> Result<(Snapshot, Params)>`

- Read `entry` → `Vec<Entry>`; `impact` + `impact_atom` (grouped by `impact.id`,
  JSON-decoding `blend`/`time`) → `Vec<Impact>`; `relation` (JSON-decoding `transform`,
  mapping `kind` text → `RelKind`) → `Vec<Relation>`; `anchor` + `kfeat` → `Palette`.
- `Params`: from the single `params` row (JSON) merged with `kfeat` (→ `KFeatMatrix`)
  and `calibration` (→ `Isotonic`). If no `params` row exists yet, return a documented
  default matching `nrs-app`'s current hardcoded `params()` (q=0.6, the five
  `k_src_levels`, `DegreeQ`, `Fixed`).
- **Taxonomy: leave `Taxonomy::default()` for now.** IMPORTANT parity note:
  `nrs-import`'s `build_snapshot` currently also leaves taxonomy empty
  (`snapshot.rs:355`), even though it has the `Contains` relations. Do **not** invent
  derivation here that the rest of the system doesn't do yet — match existing behavior;
  deriving deepest-shared-ancestor from `Contains` is a separate, later task
  (INFRA.md §5.1 end / open decision).

## Step 5 — event log + CRUD (INFRA.md §6)

Every mutation runs inside a single transaction that **also** appends one `event` row
(`kind`, `target`, `before`/`after` JSON, `ts`, `note`) — the append and the data write
must be atomic. Provide:

- `add_entry`, `add_impact`, `edit_impact`, `delete_impact`
- `add_relation`, `edit_relation`, `delete_relation`
- `upsert_anchor`, `set_kfeat_pair`
- `set_params`, `set_calibration`

Model `kind` as an `EventKind` enum with a `&'static str` mapping matching the strings
INFRA.md §5.1 lists (`impact.add`, `relation.edit`, `param.fit`, …). ULIDs for new ids.
No undo/replay engine in this pass — just the honest append (undo is a later read over
`event`).

## Step 6 — finish roadmap phase 3 (XML → nrs.db)

Add `nrs-import::write_snapshot(store: &mut Store, snap: &Snapshot) -> Result<()>` (or a
small `nrs-import` subcommand) that inserts an imported `Snapshot` into a fresh `nrs.db`,
emitting import events. This is the "→ nrs.db" half that INFRA.md phase 3 calls for and
that `nrs-import/src/lib.rs`'s doc comment flags as missing. Keep the in-memory
`build_snapshot` path intact — this writes on top of it.

## Step 7 — app integration (light; keep in scope only if cheap)

Teach `nrs-app` to load from a `.db` via `Store::load_snapshot` when passed a `nrs.db`
path (keep the existing XML path working — it can import-then-serve). Full CRUD endpoints
(the Grade/Palette views) are a **later** pass; don't build them here. Do not regress the
current read-only browsing behavior.

## Step 8 — tests

- **Round-trip**: build a small `Snapshot` (crib the `tiny_snapshot()` shape from
  `nrs-app`'s tests), write it via CRUD, `load_snapshot`, assert structural equality of
  entries/impacts/relations/palette and that `score()` gives identical values before and
  after the round-trip.
- **Event log**: each mutation appends exactly one `event` row with the right `kind` and
  a decodable `after` blob; `before` is populated on edit/delete.
- **Fresh DB**: `open` on a nonexistent path creates the schema; `load_snapshot` on an
  empty DB returns an empty `Snapshot` + default `Params`.
- Use `Store::open_in_memory()` so tests need no files.

## Out of scope (state it so scope stays honest)

- The `EntryStore` trait / `musiclib-rs` wiring (INFRA.md §7, §13: not until the second
  store is real — id-indirection is already in the schema).
- `Contains`→taxonomy derivation (parity with importer says: not yet).
- CRUD UI, `nrs-fit`, calibration UI.
- Performance work — naive is milliseconds at this scale (INFRA.md §13).

## Definition of done

`cargo test -p nrs-store` green; `cargo build --workspace` green with and without the
new `serde` feature; a `Snapshot` can make the full round trip
XML → `build_snapshot` → `write_snapshot` → `nrs.db` → `load_snapshot` → `score()` with
identical scores, and every write left an audit trail in `event`.
