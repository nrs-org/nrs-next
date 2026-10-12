# Plan: strip NRS down to scoring (titles + structural relations → type stores)

> Handoff plan. Spans four repos: `nrs/store-protocol` (spec), `musiclib-rs`,
> `nrs-anime-store-go`, and `nrs-next`. Do them in the order below; each
> step's output is the next step's input.

## Goal

`nrs.db` keeps only what NRS itself decides: impacts, relation **weights**,
judgment relations (`killed_by`), palette, params, calibration, and the event
log. Entry titles and structural relations (`contains`, `credit`, `remix`)
come from the type stores. A store can be wiped and re-ingested without NRS
losing anything.

## Decisions already made (don't re-litigate)

- **The stable identifier is a canonical URL**, not a `prefix:id` ref.
  musiclib-rs already stores provider identities as canonical URLs
  (`entry_source.identifier`, e.g. `https://musicbrainz.org/artist/<mbid>`).
  The anime store must change to match (it currently stores
  `ProviderRef.Key()` = `mal:5114` in `external_id.provider_id`). URLs also
  carry the medium (MAL anime vs manga ids collide; their URLs don't).
- **NRS `entry.id` (ULID) stays the only key used by impacts, relations and
  events.** `(store, store_id)` becomes a *cache* that is rebound from the
  URL; it is never trusted across a store wipe or a store-side dedup merge.
- **An entry has any number of bindings, each pinning one store record by
  one URL**, including several records in the same store. Two cases drive
  this. People: a seiyuu is an anime contributor and a music artist, and the
  entry should collect credits from both. Different granularity: a store can
  keep identities apart on purpose (a track and its instrumental, season 1
  and season 2) that you want to score as one entry. The only invariant is
  that one store record belongs to at most one NRS entry. NRS still does not
  mirror store aliases (schema V5 dropped `entry_alias` for this reason): it
  pins one URL per binding, not every URL the store knows.
- **Stores own structural facts; NRS owns weights.** Transforms are computed
  in NRS from `(kind, attrs)` by rules, with per-edge overrides. Evidence:
  in today's `nrs.db`, `remix` is `Scalar 0.2` on 125/125 edges,
  `feature_music` is `{music: 0.2}` on 161/161, `contains` is `1.0` on
  1109/1145 (the rest are 1/n unit-membership shares), and `credit` is already
  derived from `dah_params.roles`.
- **Scoring never calls a store.** Store edges and titles are copied into
  `nrs.db` by an explicit `sync`, which diffs and appends events. A score
  change caused by a store re-dedup must be explainable from `event`.
- **Entries can have several titles** (aliases, languages). Titles are a
  list, not one string.
- **Franchises live in a local store.** VGMDB has product pages but no API,
  so a store inside nrs-next holds them (keyed by
  `https://vgmdb.net/product/<n>`), together with the manual entries that have
  no provider at all.
- **Anisong (`feature_music`, anime → track) stays NRS-owned for now.** The
  anime store's `theme_songs[].music_link` is always `nil` today. When it is
  implemented it must hold a music **URL**, never a musiclib `store_id`
  (that would break on a musiclib wipe). Moving these edges is out of scope here.
- **`killed_by` stays NRS-owned permanently.** It is a judgment, not a fact.
- **Runtime facts come from stores; consumption facts stay in NRS.** Track
  length, episode count, episode length and VN length are facts about the
  work, so the stores own them. How much *you* consumed and how it felt
  (`episodes` watched while unfinished, `boredom`, drops) are NRS's. Today
  both kinds of number are typed into `dah_params`: 202 of 229
  `anime_consumed` impacts use the importer's guessed 20-minute episode
  length, and all 703 `music_consumed_progress` lengths and 18 VN lengths
  were entered by hand. After this change a field that the store supplies is
  left out of `dah_params`. A value that *is* present is a manual override
  and always wins.
- **Impact magnitudes stay saved in `impact_atom`.** Sync recomputes them when
  a store value changes and logs an event, the same way it treats relation
  transforms, so scoring still never calls a store.

## Step 0 — protocol v0.2 (`nrs/store-protocol/specification.md`)

All additions are optional fields or new operations, so this is a minor
version bump (§10). Advertise new operations in `capabilities.operations`.

1. **`resolve` gains `urls` and `titles`:**
   ```json
   { "store_id": "42", "title": "…", "kind": "track",
     "urls": ["https://musicbrainz.org/recording/…", "https://open.spotify.com/track/…"],
     "titles": [{"content": "薬屋のひとりごと", "language": "ja", "primary": true},
                {"content": "The Apothecary Diaries", "language": "en", "primary": false}],
     "cover_url": null, "release_date": null, "extra": {} }
   ```
   `urls` lists only canonical http(s) URLs. Bare codes (ISRC, UPC) are
   excluded. `title` stays as the primary display projection. `language`
   is a BCP-47 tag or `und`.

   **`resolve` also gains `measures`**: a flat object of runtime facts with
   fixed names and units, defined in the spec. Every key is optional; a store
   leaves out what it doesn't know, and never sends 0 to mean unknown:
   ```json
   "measures": {
     "duration_seconds": 221,          // whole work: track, movie, VN, release total
     "episode_count": 24,              // episodic works; absent while unknown/airing
     "episode_duration_seconds": 1420, // typical episode length
     "status": "finished"              // 'finished' | 'airing' | 'upcoming' | 'hiatus' | 'cancelled'
   }
   ```
   New keys are additive (minor version). NRS ignores keys it doesn't know.
2. **New op `lookup`** (synchronous, local-only, batched; never fetches from a
   provider):
   ```http
   POST /nrs-store/v1/lookup
   {"urls": ["https://youtu.be/abc", "https://myanimelist.net/anime/5114"]}
   ```
   ```json
   {"results": [
     {"url": "https://youtu.be/abc", "canonical_url": "https://youtu.be/abc",
      "entry": { …same shape as resolve… }},
     {"url": "https://myanimelist.net/anime/5114", "canonical_url": "…", "entry": null}
   ]}
   ```
   The store canonicalizes each input first, then matches against every URL
   it holds for any entry, not just a primary one. `entry: null` = unknown here.
   Results come back in input order.
3. **New op `relations`** (synchronous). Returns only **asserted** edges
   (provider-asserted or user-confirmed in the store). Unconfirmed edges stay
   in `suggest_relations`:
   ```http
   GET /nrs-store/v1/entries/{store_id}/relations
   ```
   ```json
   {"relations": [
     {"kind": "contains", "target": {"store_id": "17", "url": "https://…"},
      "attrs": {"disc_no": 1, "track_no": 3}},
     {"kind": "credit", "target": {"store_id": "9", "url": "https://…"},
      "attrs": {"roles": ["vocal", "image"]}}
   ]}
   ```
   Direction is NRS's: `from` = the entry queried, `to` = target, with the same
   meaning as `relation.from_e/to_e`. `target.store_id` is set when the
   target is in the same store. When it is in another store only `url` is set,
   and NRS finds it through `lookup` across stores. `attrs` is
   kind-specific and documented per kind in the spec.

## Step 1 — musiclib-rs

- `lookup`: canonicalize with the existing `CanonicalizeProvider` chain
  (no fetch), then `entry_source(source, identifier) → entry_id`.
- `resolve`: fill `urls` from `entry_source` (drop `isrc`/`upc`/non-URL
  identifiers), and `titles` from `entry_alias` (union over the entry's
  sources, dedupe by `(content, locale)`, `primary` from `entry_alias.primary`).
- `relations`, mapped from data musiclib already has:
  - `entry_child` → `contains` (parent → child), attrs `disc_no`, `track_no`.
  - `contribution` → `credit` (artist → work), attrs `roles`, `main_artist`.
    In current NRS data credits are music credits (`{"roles":"vocal+image"}`).
  - `entry_relation` rows with `enabled` and a non-heuristic `origin`, kind in
    `remix`/`cover`/`arrangement`/… → `remix` with attrs `{"variant": kind}`,
    direction original → derivative (match NRS's current direction; check
    against existing `remix` edges before fixing it).
  - `member_of` → `contains` (artist → unit), attrs `member_count`.
- `measures`:
  - track: `duration_seconds` = the median of `entry_source.duration_ms_all`
    across the entry's sources. Same-identity sources differ by a few
    seconds of padding; "long ver." / extended cuts are separate
    identities, so they don't pollute the median.
  - release: `duration_seconds` = the sum of its tracks' durations through
    `entry_child`, only when every track has a duration. Otherwise leave it
    out; a partial sum would be wrong, not just less precise.
- Leave `suggest_relations: false` unless it's free.

## Step 2 — nrs-anime-store-go

- Add `ProviderRef.URL()` (canonical: `https://myanimelist.net/anime/5114`,
  `https://anilist.co/manga/30013`, `https://vndb.org/v17516`, …) and store
  that in `external_id.provider_id` in place of `Key()`. Keep accepting
  short refs and legacy NRS ids **as input** (`ParseProviderRef` is unchanged).
  The repo's `anime.db` has 0 `external_id` rows. Check the DB you actually
  run (`ANIME_STORE_DB`); if it has rows, add a one-shot rewrite migration.
- `lookup`, plus `urls`/`titles` on `resolve` (titles from
  `extra.display.titles` + `synonyms`; `primary` = the current top-level `title`).
- `measures`, from the `progress_basis` the store already aggregates:
  - anime: `episode_count`, `episode_duration_seconds`, `status`. For
    `anime_movie` also `duration_seconds` (= `episode_duration_seconds`).
    `duration_seconds` for a TV series is left out; NRS multiplies
    episodes × episode length itself, because "watched" is NRS's number.
  - visual novel: `duration_seconds` = `length_minutes × 60`, `status`.
  - manga/novel: only `status` for now (`chapter_count`/`volume_count` can be
    added when an NRS formula reads them).
  - Only emit a field when aggregation picked a winner. A conflicted field
    (`data_quality.has_conflicts`) is left out rather than guessed.
- `relations`: return `[]` for now. AniList relations stay suggestions.
  Advertise the op anyway so the client path is exercised.
- Change `theme_songs[].music_link` to be a URL (field shape only; filling it
  is out of scope).

## Step 3 — `nrs-entry-store` client

- Extend the `EntryStore` trait: `lookup(&[&str]) -> Vec<LookupResult>`,
  `relations(store_id) -> Vec<StoreRelation>`; add `urls`/`titles`/`measures`
  to `EntryMeta` (`#[serde(default)]` so v0.1 stores still decode).
  `Measures` is a typed struct of `Option`s, not a `Value`.
- Gate each call on `capabilities.operations` as the existing ops do.

## Step 4 — local store (`crates/nrs-local-store`)

An in-process `EntryStore` implementation with its own SQLite file
(`local.db`), so that `nrs.db` contains no authored titles or structure.
This is authored data and is never wiped, so its ULID `store_id` is stable,
and bindings without any URL bind by `store_id` alone.

```sql
CREATE TABLE entry (id TEXT PRIMARY KEY, kind TEXT NOT NULL, url TEXT UNIQUE,
                    measures TEXT NOT NULL DEFAULT '{}');  -- same shape as the protocol's
CREATE TABLE title (entry TEXT NOT NULL REFERENCES entry(id), content TEXT NOT NULL,
                    language TEXT NOT NULL DEFAULT 'und', is_primary INTEGER NOT NULL DEFAULT 0);
CREATE TABLE relation (kind TEXT NOT NULL, from_e TEXT NOT NULL REFERENCES entry(id),
                       to_store_id TEXT, to_url TEXT, attrs TEXT NOT NULL DEFAULT '{}',
                       CHECK ((to_store_id IS NULL) <> (to_url IS NULL)));
```

Holds: VGMDB franchises/games (`F-`/`G-VGMDB`, ~100), and manual entries
(`M-<timestamp>`, 224) that end up with no store after Step 6. A manual
track's length is entered here, not in the impact, so a store-less entry
goes through the same code path as a store-backed one. Franchise ⊃
anime/album edges are cross-store, so they are written with `to_url`.

## Step 5 — `nrs-store`: schema V6 + sync

```sql
-- entry keeps only its ULID; entry.store/store_id/title_cache move out.
CREATE TABLE entry_binding (
    id         INTEGER PRIMARY KEY,     -- surrogate: url can be NULL, so it can't be part of the key
    entry      TEXT NOT NULL REFERENCES entry(id),
    store      TEXT NOT NULL,           -- 'anime' | 'music' | 'local'
    url        TEXT,                    -- pinned canonical URL (NULL only for url-less local entries)
    store_id   TEXT,                    -- cache; rebound from url
    is_primary INTEGER NOT NULL DEFAULT 0, -- exactly one per entry: whose primary title the UI shows
    measures   TEXT,                    -- cache of the store's `measures` (JSON), refreshed by sync
    bound_at   INTEGER                  -- last successful rebind
);
CREATE INDEX entry_binding_entry ON entry_binding(entry);
CREATE UNIQUE INDEX entry_binding_url ON entry_binding(store, url) WHERE url IS NOT NULL;
-- the one invariant: a store record belongs to at most one NRS entry
CREATE UNIQUE INDEX entry_binding_sid ON entry_binding(store, store_id) WHERE store_id IS NOT NULL;
CREATE UNIQUE INDEX entry_binding_primary ON entry_binding(entry) WHERE is_primary = 1;
-- title_cache is replaced by (union over bindings):
CREATE TABLE entry_title (binding INTEGER NOT NULL REFERENCES entry_binding(id) ON DELETE CASCADE,
                          content TEXT NOT NULL, language TEXT NOT NULL, is_primary INTEGER NOT NULL);
-- relation provenance:
ALTER TABLE relation ADD COLUMN origin TEXT NOT NULL DEFAULT 'nrs';  -- 'nrs' | a store name
ALTER TABLE relation ADD COLUMN attrs TEXT;                          -- store attrs (JSON)
ALTER TABLE relation ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;   -- transform hand-set; sync won't recompute
ALTER TABLE relation ADD COLUMN suppressed INTEGER NOT NULL DEFAULT 0; -- store asserts it, NRS ignores it
-- the store values an impact's saved atoms were computed from (JSON, e.g.
-- {"episode_count":24,"episode_duration_seconds":1420}); NULL = uses none
ALTER TABLE impact ADD COLUMN measures_used TEXT;
```

`binding.measures` is cached so the impact editor can compute a new impact
while the stores are down. The entry's **effective measures** are taken key
by key from its bindings: the `is_primary` binding first, then the others in
`id` order.

`load_snapshot` skips `suppressed` rows and **collapses duplicates on
`(kind, from_e, to_e)`**. If two stores assert the same credit, scoring must
see one edge, or the borrowed mass is counted twice. Collapse rule: a
`pinned` row wins, otherwise the row with the larger transform wins (log
the tie). Everything else in scoring is unchanged.

`Store::sync(stores)` does four passes, each in a transaction that writes events:

1. **Rebind.** `lookup` every binding's `url` (batched per store). On a hit,
   update `store_id`/`bound_at` (`entry.rebind` event if `store_id` changed).
   On a miss, `ingest(url)`, then lookup again. If it still misses, flag the
   binding as orphaned in the review queue and leave it intact. **When two
   bindings resolve to one `(store, store_id)`, the store has merged two
   records:**
   - both bindings belong to the same entry → collapse them into one row
     (keep `is_primary` if either had it) and append an `entry.rebind` event;
     no review needed;
   - they belong to different entries → keep the loser's old `store_id`,
     queue an `entry.merge` review (repoint impacts/relations/bindings, keep
     one ULID), and never auto-merge. **Cross-store hint:** if a URL in one binding's
   `resolve.urls` also appears in another entry's binding in a different
   store (e.g. both stores hold the same MusicBrainz artist URL), queue a
   merge suggestion. This is how a person ingested separately in each store
   gets unified.
2. **Titles and measures.** Replace each binding's `entry_title` rows and
   its `measures` cache from the lookup payload. Display title = the primary title of the entry's `is_primary` binding;
   search covers all rows. No events (titles don't affect scores).
3. **Relations.** For each binding, fetch `relations`, resolve targets
   (same-store `store_id` → the entry bound to it; foreign `url` → `lookup`
   across stores), then map both ends to NRS entries. For each (entry, store),
   take the union over all of that entry's bindings in that store, and diff it
   against rows with `origin = <that store>` on `(kind, from_e, to_e)`.
   Stores are diffed independently, so one store dropping an edge does not
   delete the copy another store asserts. **Drop self-loops** (`from_e ==
   to_e`), and report them. They are expected when one entry binds both ends,
   e.g. musiclib asserts instrumental → original and you bound both to one
   entry. Then apply:
   - new edge → insert with `transform = rule(kind, attrs)`;
   - gone → delete, unless `pinned` (then keep it and queue a review);
   - attrs changed → recompute the transform unless `pinned`.
   Each change appends one `relation.sync.{add,remove,update}` event with
   before/after. Unresolvable targets are reported, not inserted.
4. **Measures → impacts.** For each impact whose kind reads a measure and
   whose `dah_params` leaves that field to the store, rebuild its atoms from
   `(dah_kind, dah_params, effective measures)`:
   - the measures it needs are unchanged from `measures_used` → do nothing;
   - they changed → replace `impact_atom` rows, update `measures_used`, and
     append one `impact.remeasure` event (before/after atoms and measures);
   - a needed measure is missing → keep the saved atoms and `measures_used`
     as they are, and queue a `measure_missing` review. Never compute with a
     default (the 20-minute guess is exactly what this removes).

Rules live in one place (`nrs-store::rules`, or `Params` if they should
be fittable later): `contains` → `Scalar(1/member_count)` if
`member_count` else `1.0`; `remix` → `0.2`; `credit` → the existing DAH
role mapping in `nrs-import/src/roles.rs`. `killed_by`/`feature_music` never
come from stores.

**Impact formulas move to one place.** The `kind + params → atoms` dispatch
currently lives in `nrs-app/src/main.rs` (`build_impact_atoms`). Move it into
`nrs_import::dah::build(kind, &params, &Measures) -> Result<Vec<Atom>, MissingMeasure>`
so the app and sync share it. Each kind declares which params can come from
the store:

| `dah_kind` | param (minutes) | when absent, from | notes |
| --- | --- | --- | --- |
| `anime_consumed` | `episode_duration_minutes` | `episode_duration_seconds / 60` | |
| `anime_consumed` | `episodes` | `episode_count` when the param is `"all"` | a number = progress so far, always NRS's |
| `consumed` | `duration_minutes` | `duration_seconds / 60` | VNs, movies |
| `music_consumed_progress` | `duration_minutes` | `duration_seconds / 60` | |

`episodes: "all"` is the new way to write "finished it". It follows the
store if the count is corrected later (or once an airing show completes).
`dropped` and the others read no measures and are untouched.

## Step 6 — migrate the existing `nrs.db`

Report first, apply second. Nothing is deleted until the report has been read.

1. **Entries → one initial binding each** (`is_primary=1`), by current `store`:
   - `mal` (228): `https://myanimelist.net/anime/<id>` → anime. `L-MAL`,
     `V-VNDB` → anime (manga / VN URLs).
   - `url-*` (~230): the URL as-is → music, via `ingest`, then re-pin to
     the canonical URL `lookup` returns. Shortlinks (`linkco.re`, `lnk.to`,
     `fanlink.to`, …) are never pinned: re-pin to the most durable URL in
     `resolve.urls` (MusicBrainz > VGMDB > Spotify > Apple Music > YouTube).
   - `M-VGMDB-AL/AR` (768): try `lookup` with the `vgmdb.net/album|artist`
     URL (musiclib already holds `vgmdb` sources as cross-links) and pin the
     MusicBrainz URL if one exists. Report misses; they wait for the
     VGMDB→MusicBrainz work in musiclib-rs.
   - `F-`/`G-VGMDB` → local store with the `vgmdb.net/product/<n>` URL.
   - `M-<timestamp>`/`O-…` (manual) → local store, unless the report shows a
     usable URL for one.
   - `title_cache` → `entry_title` (primary). For local entries the title is
     moved into `local.db` instead.
   - Extra bindings are not created by the migration. They come later from
     the cross-store merge suggestions in Step 5 or are added by hand (e.g.
     binding a VGMDB artist entry to its anime-store contributor, or binding
     an instrumental's musiclib record to the original's entry).
2. **Relations.** Run sync in dry-run mode, then classify every existing edge:
   - a store asserts it and `rule == transform` → `origin=<store>`;
   - a store asserts it but the transform differs → `origin=<store>, pinned=1`;
   - the store doesn't assert it → keep as `origin='nrs'` and list it, so you
     can decide edge by edge whether to add it in the store (or the local store);
   - `killed_by`, `feature_music` → `origin='nrs'`, untouched.
3. **Measures.** For each measure-reading impact, compare each param with the
   store's effective measure and classify it:
   - `anime_consumed.episode_duration_minutes == 20` (202 of 229): these are
     the importer's fallback, not your input. Drop the param so the store
     value is used.
   - any other param that equals the store value (±0.5 min for episodes,
     ±2 s for tracks, ±1 min for VN length) → drop it.
   - `anime_consumed.episodes == episode_count` → rewrite as `"all"`.
   - differs from the store → keep it as a manual override, and list it so
     you can decide whether the store or your number is wrong.
   - the store has no value → keep it. If the entry is local, move the value
     into the local store's `measures` and drop the param.
   The report shows the magnitude change for every impact. Most changes are
   tiny (the `consumed` formula scales with `ratio^0.05`), but **a total that
   crosses 10 or 120 minutes jumps** between `consumed`'s base tiers, so sort
   by that first.
4. **Score parity check:** `score()` over the whole snapshot before and after
   must match except for edges and impacts the report listed as changed.

## Step 7 — strip authoring paths

- `nrs-import` and `nrs-app` stop writing titles and structural relations into
  `nrs.db`. The entry page reads `entry_title` and shows the relation's `origin`
  (store edges are read-only apart from pin/suppress).
- Impact editor: a measure-backed field shows the store value as its default,
  with an explicit "override" control. Saving with the field blank leaves
  the param out (`normalize_duration_fields` must accept blank). `episodes`
  gets a "finished (all)" option. If the store has no value, the field is
  required, as it is today.
- Drop `entry.store`/`store_id`/`title_cache` once nothing reads them (SQLite ≥
  3.35 `DROP COLUMN`; the `UNIQUE(store, store_id)` constraint means a table
  rebuild rather than a plain `DROP COLUMN`).

## Out of scope

- Moving anisong edges into the anime store (`music_link` filling + `relations`).
- Anime-store asserted relations (franchise/sequel); AniList stays suggestion-only.
- VGMDB → MusicBrainz migration inside musiclib-rs.
- Auto-merging NRS entries; merges are always reviewed.

## Definition of done

Delete `musiclib.db` and `anime.db`, re-ingest nothing by hand, run
`nrs sync`: every entry rebinds from its URL (via `ingest` on miss), titles
and measures repopulate, store relations are recreated, and `score()` is
identical to before the wipe, apart from changes that appear as
`relation.sync.*` or `impact.remeasure` events.
`cargo test --workspace` and `go test ./...` are green, and both stores report
`protocol_version: "0.2"`.
