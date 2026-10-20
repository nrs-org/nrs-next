# nrs-next development

NRS is a Rust workspace with a local browser UI. Use the Nix and
process-compose launcher in this repository to manage its services.

## Running the stack

Check `./dev status` first and reuse the running stack. Use these commands
instead of starting separate unmanaged copies of the servers:

```sh
./dev up -D               # start all five services in the background
./dev status              # current processes and readiness
./dev attach              # process-compose terminal dashboard
./dev logs nrs-app        # build and application output for one service
./dev restart nrs-app     # rebuild and restart the changed service
./dev down                # stop the managed stack
./dev check               # validate configuration without starting services
./dev version             # process-compose version
```

`./dev` without arguments starts the stack with its terminal dashboard.
It enters the pinned Nix shell automatically. `process-compose.yaml` defines
the processes, readiness checks, dependencies, and restart policies;
`scripts/dev-stack.py` contains their build and launch commands. This setup
runs native processes and uses existing databases and build caches.

| Service | Source checkout | Default port |
| --- | --- | --- |
| `nrs-app` | This repository | 3737 |
| `anime-store` | `../nrs-anime-store-go` | 3738 |
| `musiclib` | `../musiclib-rs` | 4600 |
| `ytdlp` | `../musiclib-rs` | 3000 |
| `ytmusicapi` | `../musiclib-rs` | 9001 |

The app waits for both stores to become healthy; musiclib waits for both
helpers. Open `http://127.0.0.1:3737/` for the UI. Logs and the supervisor's
Unix socket live in the ignored `.dev-stack/` directory.

## Edit, build, and refresh

Keep the stack running and restart only the affected service after backend
changes. Cargo and Go reuse their build caches; a restart checks for changes
and builds before launching the new executable.

| Changed code | Action |
| --- | --- |
| `crates/nrs-app/assets/` HTML, CSS, or JavaScript | Refresh the browser; no build or server restart |
| NRS Rust crates, including scoring and persistence | `./dev restart nrs-app` |
| Musiclib player HTML/CSS/JS (`../musiclib-rs/src/bin/server/frontend/index.html`) | `./dev restart musiclib`, then refresh the browser in the default `release` profile |
| Musiclib server or library Rust code | `./dev restart musiclib` |
| Anime store Go code | `./dev restart anime-store` |
| yt-dlp wrapper Rust code | `./dev restart ytdlp` |
| ytmusicapi Python script | `./dev restart ytmusicapi` |

Musiclib embeds its player HTML in release binaries. Its `dev` builds read the
HTML from the source tree on each request, so player asset edits in that
profile only need a browser refresh. Verify the served `/player` page after
restarting a release build; an unbuilt source fix is not yet live.

The default Rust profile is `release`. For frequent Rust edits, start the
supervisor with the `dev` profile for faster compilation:

```sh
./dev down
NRS_BUILD_PROFILE=dev ./dev up -D
```

The supervisor retains its startup environment. Set the profile when starting
the stack; setting it only on a later `./dev restart` client invocation does
not change the running supervisor's environment. Switching profiles may need
an initial build into the separate Cargo profile cache.

Process-compose is pinned to 1.122.0 and supports file watching. Automatic
rebuilds on save are not configured in `process-compose.yaml`; use the manual
restart workflow above. Changes to Nix dependencies or the launch environment
require stopping and starting the stack to use the new environment.

yt-dlp and ytmusicapi are built from upstream source in `nix/scrapers.nix`.
Update them with `scripts/update-scrapers.py`, not by updating the main
`nixpkgs` pin; the script also keeps the yt-dlp-ejs input at yt-dlp's pin.

## Data and configuration

- The selected NRS database is `nrs-strip.db`. Set `NRS_DB_PATH` to select
  another existing database; the launcher refuses to create a missing one.
- Anime metadata defaults to `~/.local/share/nrs-anime-store/anime.db`
  (`$XDG_DATA_HOME/nrs-anime-store/`). Musiclib uses
  its normal platform data directory (`~/.local/share/musiclib-rs/` on Linux).
- Each store loads its own checkout's `.env`; inherited environment variables
  take precedence. Musiclib also reads its normal platform configuration.
- The launcher wires service URLs to the managed local ports. See `README.md`
  for port, checkout, data path, and state-directory overrides.
- Occupied ports are reported instead of taking over existing services. Use
  the supervisor commands to inspect and stop its own services.
- Keep databases, backups, credentials, logs, and build output out of commits.
  For integration checks that write data, use database snapshots and separate
  ports/state directories.

Use the Git-backed `nix develop` or `nix develop .` when running tools directly.
`nix develop path:.` includes ignored local data in source snapshots and can
copy large database backups into the Nix store.

## Workspace and verification

- `nrs-core`: scoring engine, without I/O or store dependencies.
- `nrs-store`: NRS persistence and event log; entry references are opaque IDs.
- `nrs-entry-store`: HTTP client for the NRS Store Protocol.
- `nrs-import`: legacy imports and synchronization tools.
- `nrs-app`: local Axum API and dependency-free browser client.

Run commands inside `nix develop` or prefix them with `nix develop -c`:

```sh
cargo check -p nrs-app
cargo test -p nrs-app         # API/backend changes
cargo test -p nrs-core        # scoring changes
git diff --check
```

Choose checks appropriate to the changed code. For frontend asset changes,
refresh the running UI and verify the affected interaction. For launcher
changes, run `./dev check` and verify the affected lifecycle behavior.
