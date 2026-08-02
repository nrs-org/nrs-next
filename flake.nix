{
  description = "nrs-next dev environment";

  # Pinned to this machine's current NixOS nixos-unstable revision (see
  # /etc/nixos/flake.lock, nixpkgs input) so the dev shell reuses store
  # paths already fetched for the system instead of pulling a second copy.
  inputs.nixpkgs.url = "github:nixos/nixpkgs/d407951447dcd00442e97087bf374aad70c04cea";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          cargo
          rustc
          rust-analyzer
          rustfmt
          clippy
          sqlite
          pkg-config
          uv
          python3
        ];

        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    };
}
