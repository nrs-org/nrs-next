{ lib, craneLib }:

let
  commonArgs = {
    pname = "nrs-app";
    inherit ((lib.importTOML ../Cargo.toml).workspace.package) version;
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../crates
      ];
    };
    strictDeps = true;
    cargoExtraArgs = "--locked -p nrs-app";
  };

  # Dependencies only change with Cargo.lock, so source edits reuse this build.
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;

    # The server serves its browser client from this directory (see main.rs).
    env.NRS_APP_ASSETS_DIR = "${placeholder "out"}/share/nrs-app/assets";
    postInstall = ''
      mkdir -p $out/share/nrs-app
      cp -r crates/nrs-app/assets $out/share/nrs-app/assets
    '';

    meta = {
      description = "Local NRS scoring API and browser client";
      mainProgram = "nrs-app";
    };
  }
)
