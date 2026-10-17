{
  description = "nrs-next dev environment";

  # Pinned to this machine's current NixOS nixos-unstable revision (see
  # /etc/nixos/flake.lock, nixpkgs input) so the dev shell reuses store
  # paths already fetched for the system instead of pulling a second copy.
  inputs.nixpkgs.url = "github:nixos/nixpkgs/d407951447dcd00442e97087bf374aad70c04cea";
  # The anime store requires Go 1.26.7; keep the existing Rust environment
  # pinned while taking Go and the supervisor build from this newer revision.
  inputs.nixpkgs-go.url = "github:NixOS/nixpkgs/b1b875982b17dabde9b4a37f3e229e74913e6db3";

  outputs =
    { self, nixpkgs, nixpkgs-go }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      goPkgs = import nixpkgs-go { inherit system; };
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
          yt-dlp
          curl
          (python3.withPackages (ps: [ ps.ytmusicapi ps.python-dotenv ]))
        ];

        NRS_STACK_NIX = "1";
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
      };
    };
}
