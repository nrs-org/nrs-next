{
  description = "nrs-next dev environment";

  # Pinned to this machine's current NixOS nixos-unstable revision (see
  # /etc/nixos/flake.lock, nixpkgs input) so the dev shell reuses store
  # paths already fetched for the system instead of pulling a second copy.
  inputs.nixpkgs.url = "github:nixos/nixpkgs/d407951447dcd00442e97087bf374aad70c04cea";
  # The anime store requires Go 1.26.7; keep the existing Rust environment
  # pinned while taking Go and the supervisor build from this newer revision.
  inputs.nixpkgs-go.url = "github:NixOS/nixpkgs/b1b875982b17dabde9b4a37f3e229e74913e6db3";
  inputs.crane.url = "github:ipetkov/crane";
  # yt-dlp and ytmusicapi follow YouTube changes, so nix/scrapers.nix builds
  # them from upstream source on the main pin. scripts/update-scrapers.py
  # updates these inputs and keeps the yt-dlp-ejs wheel at yt-dlp's pin.
  inputs.yt-dlp-src = { url = "github:yt-dlp/yt-dlp"; flake = false; };
  inputs.ytmusicapi-src = { url = "github:sigma67/ytmusicapi"; flake = false; };
  inputs.yt-dlp-ejs-whl = {
    url = "https://files.pythonhosted.org/packages/py3/y/yt-dlp-ejs/yt_dlp_ejs-0.8.0-py3-none-any.whl";
    flake = false;
  };

  outputs =
    { self, nixpkgs, nixpkgs-go, crane, yt-dlp-src, ytmusicapi-src, yt-dlp-ejs-whl }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      goPkgs = import nixpkgs-go { inherit system; };
      scrapers = import ./nix/scrapers.nix {
        inherit pkgs yt-dlp-src ytmusicapi-src yt-dlp-ejs-whl;
        ejsUrl = (builtins.fromJSON (builtins.readFile ./flake.lock)).nodes.yt-dlp-ejs-whl.original.url;
      };
      # Pin the supervisor independently of the Rust/Go environments. Source
      # and module hashes match Nixpkgs' process-compose 1.122.0 package.
      processCompose = goPkgs.process-compose.overrideAttrs (finalAttrs: previous: {
        version = "1.122.0";
        src = previous.src.override {
          tag = "v${finalAttrs.version}";
          hash = "sha256-K1j4hJE23Wxf7P/ipMkMl/3uOtdE8Xj7LfyjvwC2rMI=";
        };
        vendorHash = "sha256-Wl3vMR122wn5ivQVbdS79nATQBlEHu44h0OZLGeEasU=";
      });
    in
    {
      packages.${system} = rec {
        nrs-app = pkgs.callPackage ./nix/nrs-app.nix { craneLib = crane.mkLib pkgs; };
        default = nrs-app;
        inherit (scrapers) yt-dlp ytmusicapi-python;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          cargo
          rustc
          rust-analyzer
          rustfmt
          clippy
          cmake
          clang
          llvmPackages.libclang
          sqlite
          pkg-config
          openssl
          # musiclib's `ffi-system` feature links this prebuilt libffi.
          libffi
          uv
          goPkgs.go
          processCompose
          scrapers.yt-dlp
          curl
          scrapers.ytmusicapi-python
        ];

        NRS_STACK_NIX = "1";
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
      };
    };
}
