# nrs-next

Rust implementation of NRS (personal anime/manga/music scoring system),
per `../INFRA.md`.

## Dev environment

`direnv allow` (or `nix develop`) picks up `flake.nix` — rustc/cargo/clippy/
rustfmt/rust-analyzer, pinned to this machine's nixos-unstable revision.

## Crates

- `nrs-core` — pure scoring engine (INFRA.md §4). No I/O, no store deps.
  **Keystone.** Currently: frozen `Snapshot`/`Params` types; `score`/`explain`
  are stubs pending the math (roadmap phase 2).

The rest of the crate layout (`nrs-store`, `nrs-fit`, `nrs-app`,
`nrs-import`, external `stores/musiclib-rs`) lands per INFRA.md §12 as later
phases are reached.
