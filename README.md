# nrs-next

Rust implementation of NRS (personal anime/manga/music scoring system),
per `../INFRA.md`.

## Dev environment

`direnv allow` (or `nix develop`) picks up `flake.nix` — rustc/cargo/clippy/
rustfmt/rust-analyzer, pinned to this machine's nixos-unstable revision.

## Crates

- `nrs-core` — pure scoring engine (INFRA.md §4). No I/O, no store deps.
- `nrs-store` — the central `nrs.db` persistence and event log. It stores only
  opaque `(store, store_id)` references for entries.
- `nrs-entry-store` — client for the language-independent NRS Store Protocol.
- `nrs-import` — legacy XML import and DAH formula compatibility.
- `nrs-app` — the local Axum UI and CRUD surface.

## Entry stores

Anime, manga, novel, and visual-novel metadata is provided by the Go service
in `../nrs-anime-store-go`; music metadata by `../musiclib-rs`'s `server`.
Start both separately, then launch `nrs-app` against an `nrs.db`:

```sh
(cd ../nrs-anime-store-go && go run .)
(cd ../musiclib-rs && cargo run --release --bin server)
cargo run -p nrs-app -- /path/to/nrs.db
```

The app defaults to `http://127.0.0.1:3738` (anime) and
`http://127.0.0.1:4600` (music); set `NRS_ANIME_STORE_URL` /
`NRS_MUSIC_STORE_URL` to use different Store Protocol endpoints. The entries page accepts provider URLs and
IDs and persists only the opaque store ID returned by the service. Provider
identity, provenance, metadata aggregation, and deduplication remain
in the Go store.
