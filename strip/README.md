# Stripped XML

`nrs-impl/nrs-strip/` is `nrs-impl/nrs/` reduced to what NRS itself decides.
The type stores supply the rest when the importer reads it.

| In the stripped XML | From the stores |
|---|---|
| impacts, roles, `musicvar`, `bestGirl` | titles (musiclib, anime store) |
| `remix`, `featureMusic`, `killedBy` | album → track `contains` (musiclib) |
| franchise/artist grouping, any `contains` the store doesn't assert | |
| `<bind url>`: which store record an entry is | |
| progress lengths/episodes the stores can't know (partial watches, osu! cuts) | track lengths (musiclib), episode counts and movie/short lengths (anime store) |
| `<unlink id>`: a store edge NRS refuses | |

Rules the generator and the importer share (`crates/nrs-import/src/stores.rs`):

- A store `release → track` edge becomes a `contains` edge only when both
  ends map one-to-one onto NRS entries, the parent doesn't `<unlink>` it,
  and the XML doesn't already declare it.
- An entry keeps its `title` attribute when no store can supply one
  (VGMDB-only, osu!, local entries). Otherwise the title becomes a comment.
- A store title never feeds a scoring rule. Featuring ("feat." in a legacy
  title) is pinned as `<musicvar feat="true"/>`.
- Progress: `<musicConsumedProgress/>` without `length` takes the bound
  track's musiclib length (the bound source's own length, else MusicBrainz,
  else Spotify, else any). `<animeConsumedProgress>` without `episodes`
  takes the anime store's episode count; without `episodeDuration` it counts
  20 minutes per TV episode (OP/ED excluded), or the store's length when
  that is under 15 or 30+ minutes (movies, specials, shorts). The generator
  drops a music `length` within 10 s of the store's (not
  `generatedBy="user"` ones), `episodes` equal to the store's count, and an
  `episodeDuration` within a minute of what the importer would use.
- Nested entries under an entry that runs scripts stay nested, because
  entry-level `local` script state flows into child scopes and scripts see
  document order.

## Regenerate

```sh
python3 -I strip/build_bindings.py   # -> strip/bindings.tsv (entry -> URL)
python3 -I strip/strip_xml.py        # ../nrs-impl/nrs -> ../nrs-impl/nrs-strip
```

`build_bindings.py` reads the migration ledgers in `result-vgmdb-musicbrainz/`,
`nrs.db` and `musiclib.db`.

## Check and import

```sh
cargo run -p nrs-import --features scripting --bin strip_check -- \
  ../nrs-impl/nrs/NRSImplMain.xml ../nrs-impl/nrs-strip/NRSImplMain.xml \
  --music-store http://127.0.0.1:4600 --anime-store http://127.0.0.1:3738

NRS_MUSIC_STORE_URL=http://127.0.0.1:4600 NRS_ANIME_STORE_URL=http://127.0.0.1:3738 \
  cargo run -p nrs-import --features scripting --bin import -- \
  ../nrs-impl/nrs-strip/NRSImplMain.xml new.db
```

`resync` takes `--music-store` / `--anime-store` (or the same environment
variables), and nrs-app's XML mode reads the environment variables. Both
stores are Store Protocol servers: musiclib's is `../musiclib-rs`'s `server`
binary, which runs without provider credentials (ingest is then disabled;
NRS only needs `lookup`, `resolve` and `suggest_relations`).

`strip_check --tolerance 0.01` pairs impacts that only differ slightly in
magnitude (store lengths a few seconds off the written ones). As of
2026-10-13 that leaves 4 differences: Isekai Quartet, Isekai Quartet 2,
Another World and Midara na Ao-chan, short-episode anime that used to count
20 minutes per episode and now take the store's 10-12 (boredom -2.6% to
-3.4%).

Before the progress rule, `strip_check` found the two snapshots identical (1,570
entries, 3,984 impacts, 2,076 relations), once the duplicate declarations of
albums `M-20240314T134125` and `M-VGMDB-AL-116264` were collapsed into one
each. A duplicate declaration double-counts its impacts and nested edges.
