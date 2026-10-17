# yt-dlp and ytmusicapi built from their upstream branches on this flake's
# main nixpkgs pin. Updates change only these sources, so Python, Deno, and the
# rest of the closure stay shared with the system. Update them with
# scripts/update-scrapers.py.
{
  pkgs,
  yt-dlp-src,
  ytmusicapi-src,
  yt-dlp-ejs-whl,
  # The wheel's locked URL; its file name carries the version.
  ejsUrl,
}:
let
  inherit (pkgs) lib;
  date = src: builtins.substring 0 8 src.lastModifiedDate;

  ytdlpVersion =
    let
      lines = lib.splitString "\n" (builtins.readFile "${yt-dlp-src}/yt_dlp/version.py");
      matches = lib.filter (m: m != null) (map (builtins.match "__version__ = '([^']+)'") lines);
    in
    builtins.head (builtins.head matches);

  # yt-dlp requires one exact yt-dlp-ejs release (its YouTube challenge solver).
  ejsRequired =
    let
      project = (builtins.fromTOML (builtins.readFile "${yt-dlp-src}/pyproject.toml")).project;
      pin = lib.findFirst (lib.hasPrefix "yt-dlp-ejs==") null project.optional-dependencies.default;
    in
    if pin == null then
      throw "yt-dlp's pyproject.toml no longer pins yt-dlp-ejs; update nix/scrapers.nix"
    else
      lib.removePrefix "yt-dlp-ejs==" pin;
  ejsVersion = builtins.head (builtins.match ".*/yt_dlp_ejs-([^-]+)-py3-none-any\\.whl" ejsUrl);

  python = pkgs.python3.override {
    self = python;
    packageOverrides = pyFinal: pyPrev: {
      # The PyPI wheel ships the built JavaScript, so no Node or pnpm build.
      yt-dlp-ejs = pyFinal.buildPythonPackage {
        pname = "yt-dlp-ejs";
        version = ejsVersion;
        format = "wheel";
        src = yt-dlp-ejs-whl;
        unpackPhase = ''
          mkdir dist
          cp $src dist/yt_dlp_ejs-${ejsVersion}-py3-none-any.whl
        '';
        pythonImportsCheck = [ "yt_dlp_ejs" ];
      };

      ytmusicapi = pyPrev.ytmusicapi.overridePythonAttrs (old: {
        version = "0-unstable-${date ytmusicapi-src}";
        src = ytmusicapi-src;
        # setuptools-scm reads the version from Git tags, which the source lacks.
        env = (old.env or { }) // {
          SETUPTOOLS_SCM_PRETEND_VERSION = "0+unstable.${date ytmusicapi-src}.g${ytmusicapi-src.shortRev}";
        };
        meta = old.meta // {
          changelog = "https://github.com/sigma67/ytmusicapi/commits/${ytmusicapi-src.rev}";
        };
      });
    };
  };

  # musiclib's helper only dumps metadata JSON: no downloads, post-processing,
  # browser impersonation, or cookie decryption.
  yt-dlp =
    (pkgs.yt-dlp.override {
      python3Packages = python.pkgs;
      ffmpegSupport = false;
      atomicparsleySupport = false;
      rtmpSupport = false;
      withSecretStorage = false;
    }).overridePythonAttrs
      (old: {
        version = "${ytdlpVersion}-unstable-${date yt-dlp-src}";
        src = yt-dlp-src;
        dependencies = with python.pkgs; [
          brotli
          certifi
          requests
          urllib3
          yt-dlp-ejs
        ];
        postPatch = ''
          substituteInPlace yt_dlp/version.py \
            --replace-fail "UPDATE_HINT = None" 'UPDATE_HINT = "Run scripts/update-scrapers.py in nrs-next."'
          substituteInPlace yt_dlp/utils/_jsruntime.py \
            --replace-fail "path = _determine_runtime_path(self._path, 'deno')" "path = '${lib.getExe pkgs.deno}'"
        '';
        # Skip the man page and completions; they need pandoc to build.
        outputs = [ "out" ];
        nativeBuildInputs = [ ];
        postBuild = "";
        postInstall = "";
        meta = old.meta // {
          changelog = "https://github.com/yt-dlp/yt-dlp/commits/${yt-dlp-src.rev}";
        };
      });
in
assert lib.assertMsg (ejsRequired == ejsVersion)
  "yt-dlp needs yt-dlp-ejs ${ejsRequired}, but flake.nix pins ${ejsVersion}. Run scripts/update-scrapers.py.";
{
  inherit yt-dlp;
  ytmusicapi-python = python.withPackages (ps: [
    ps.ytmusicapi
    ps.python-dotenv
  ]);
}
