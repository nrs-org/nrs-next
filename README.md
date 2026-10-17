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
The development launcher manages these along with musiclib's yt-dlp and
ytmusicapi helpers. Keep the three checkouts next to each other, then run:

```sh
./dev
```

This enters the pinned Nix shell automatically, builds the services, and opens
Process Compose's terminal UI. Open `http://127.0.0.1:3737/` once `nrs-app` is
healthy. Stores start before the app; the music store waits for both helpers.
Failed services restart up to three times, with a three-second backoff.
Readiness checks use local HTTP responses and do not fetch provider metadata.
The first start may take several minutes to fetch Nix packages and compile;
subsequent starts reuse Cargo and Go caches. Services rebuild when restarted.
NRS frontend asset edits only require a browser refresh. Musiclib's player
HTML is embedded in release builds, so run `./dev restart musiclib` and then
refresh after editing it; musiclib's `dev` builds read the HTML on each request.

Process Compose is pinned to 1.122.0. File watching is available but is not
configured; the current development loop uses explicit service restarts.
See `CLAUDE.md` for the edit/build workflow and how to select the Rust `dev`
profile for faster compilation.

```sh
./dev up -D              # run in the background, including after this shell exits
./dev status             # service status and health
./dev attach             # open the running stack's terminal UI
./dev logs musiclib      # inspect startup, build, and application logs
./dev restart musiclib   # rebuild and restart one service
./dev down               # stop the managed services in reverse dependency order
./dev check              # validate configuration without starting services
```

The launcher defaults to the existing `nrs-strip.db`, the anime store's
`anime.db`, and musiclib's normal platform data/config/cache directories. It
refuses to create a missing NRS database or take over occupied service ports;
stop separately launched servers before the first managed start. Logs, the
supervisor socket, and the Go executable live in the ignored `.dev-stack/`.
Stopping the stack leaves databases and caches in place.

Both stores read their own checkout's `.env`; inherited environment variables
take precedence. The music store's helper URLs and the app's store URLs are
wired to the managed local ports. Python, ytmusicapi, yt-dlp, and build tools
come from Nix, so starting helpers needs no separate Python installation or
`uv` download. Go is pinned separately at the anime store's compatible nixpkgs
revision without changing this project's Rust toolchain.

Overrides are environment variables. Launcher paths resolve from `nrs-next`;
`ANIME_STORE_DB` follows the anime store's working directory:

| Variable | Default |
| --- | --- |
| `NRS_DB_PATH` | `nrs-strip.db` (must exist) |
| `NRS_ANIME_STORE_DIR` | `../nrs-anime-store-go` |
| `NRS_MUSICLIB_DIR` | `../musiclib-rs` |
| `ANIME_STORE_DB` | Anime store's existing configuration, otherwise `anime.db` in its checkout |
| `NRS_MUSIC_STORE_DB` | Musiclib's normal data directory |
| `NRS_BUILD_PROFILE` | `release` (use `dev` for faster Rust builds) |
| `NRS_APP_PORT` | `3737` |
| `NRS_ANIME_STORE_PORT` | `3738` |
| `NRS_MUSIC_STORE_PORT` | `4600` |
| `NRS_YTDLP_PORT` | `3000` |
| `NRS_YTMUSICAPI_PORT` | `9001` |
| `NRS_STACK_STATE_DIR` | `.dev-stack` (use a separate directory for a second stack) |

For example, `NRS_DB_PATH=nrs.db ./dev` selects the other NRS database. Pass the
same state-directory override to subsequent `status`, `attach`, and `down`
commands when running a second stack. Process Compose configuration lives in
`process-compose.yaml`; `./dev` also forwards other Process Compose commands.

### Store endpoints

The app defaults to `http://127.0.0.1:3738` (anime) and
`http://127.0.0.1:4600` (music); set `NRS_ANIME_STORE_URL` /
`NRS_MUSIC_STORE_URL` to use different Store Protocol endpoints. The entries page accepts provider URLs and
IDs and persists only the opaque store ID returned by the service. Provider
identity, provenance, metadata aggregation, and deduplication remain
in the Go store.
