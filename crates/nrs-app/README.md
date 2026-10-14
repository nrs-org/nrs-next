# nrs-app

Run `cargo run -p nrs-app -- nrs.db [port]` (default port 3737), then open
`http://127.0.0.1:3737/`. The UI is a dependency-free browser client in
`assets/` (`index.html`, `app.js`, `app.css`, native ES modules). Edit those
files and refresh; frontend changes need no build step or server restart. The
server serves the client and the versioned JSON API from the same origin; any
non-API path falls back to `index.html` so client routes survive a reload.

The client covers scores, entry search and ingestion, impacts, relations,
palette, pairwise calibration, K (pooling exponents), and the review queue.

## API

| Route | Methods | Purpose |
| --- | --- | --- |
| `/api/v1/scores` | GET | Live ranking; `sort`, `dir`, `q`, time overrides `shared` (λ) and `length` (L days), and a comparison run `cmp_gen` / `cmp_shared` / `cmp_length` for rank changes |
| `/api/v1/scores.csv` | GET | CSV from the same live ranking |
| `/api/v1/entries` | GET | Search and pagination; `q`, `offset`, `limit` |
| `/api/v1/entries/ingest` | POST | Ingest through the configured anime store |
| `/api/v1/entries/{entry}` | GET | Entry, impacts, and relations |
| `/api/v1/entries/{entry}/score` | GET | Score breakdown at the stored time settings: V, overall and per-type rank, each anchor's share of V (own vs borrowed), and each anchor's share split across the own impacts and lender entries' impacts whose mass reaches it |
| `/api/v1/entries/{entry}/impacts` | POST | Add a standards impact |
| `/api/v1/entries/{entry}/impacts/{impact}` | PUT, DELETE | Edit or remove a standards impact |
| `/api/v1/entries/{entry}/impacts/{impact}/raw` | PUT | Review source atoms and note while preserving imported time supports |
| `/api/v1/entries/{entry}/impacts/{impact}/confirm` | POST | Confirm review flags and suggested comments |
| `/api/v1/entries/{entry}/relations` | POST | Add a standards relation |
| `/api/v1/entries/{entry}/relations/{relation}` | PUT, DELETE | Edit or remove a relation |
| `/api/v1/palette` | GET | Anchor list and live similarity matrix |
| `/api/v1/calibration/pair` | GET, POST | Fetch a pair or record a preference |
| `/api/v1/calibration/time` | PUT | Store the default time settings `{shared, length_days}` |
| `/api/v1/calibration/kfeat` | GET, PUT | Read K (anchor exponents, pair exponents, implied corrections) or set one exponent in (0, 1] |
| `/api/v1/review` | GET | Entries awaiting review |

Standards writes use the existing form-field shape (`{"kind":"nei","factor":"0.5",...}`).
Dates and media durations are normalized by the backend. Imported relations
without editable standards metadata expose their existing transform and can be
removed; they are not rebuilt with default weights. Database mode is required
for curation. Scores and palette also support read-only XML mode.

Server tests: `cargo test -p nrs-app`.
