#!/usr/bin/env python3
"""Update yt-dlp and ytmusicapi to upstream HEAD and smoke-test them.

Moves the yt-dlp-src and ytmusicapi-src flake inputs, points the
yt-dlp-ejs-whl input at the exact yt-dlp-ejs release yt-dlp pins, rebuilds
both packages, and checks them against YouTube with musiclib's helper calls.
Needs Nix and Python 3.11 or newer; pass --no-update to only run the checks.
"""

import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FLAKE = ROOT / "flake.nix"
EJS_URL = re.compile(r"(/yt-dlp-ejs/yt_dlp_ejs-)([^-\"]+)(-py3-none-any\.whl)")
# The arguments musiclib's ytdlp_server passes, on a long-lived video.
YTDLP_ARGS = ["--dump-single-json", "--flat-playlist", "--lazy-playlist",
              "--no-check-formats", "--extractor-retries", "0",
              "https://www.youtube.com/watch?v=jNQXAC9IVRw"]
YTMUSICAPI_CHECK = """
from ytmusicapi import YTMusic
yt = YTMusic()
artist = yt.search("Rick Astley", filter="artists", limit=1)[0]
print(yt.get_artist(artist["browseId"])["name"])
"""
# Without these, extraction can succeed while YouTube formats go missing.
JS_FAILURES = re.compile(r"challenge|JavaScript runtime|ejs", re.IGNORECASE)


def nix(*args):
    return subprocess.run(["nix", *args], cwd=ROOT, check=True, text=True,
                          stdout=subprocess.PIPE).stdout


def update():
    nix("flake", "update", "yt-dlp-src", "ytmusicapi-src")
    inputs = json.loads(nix("flake", "archive", "--json"))["inputs"]
    pyproject = Path(inputs["yt-dlp-src"]["path"]) / "pyproject.toml"
    pins = tomllib.loads(pyproject.read_text())["project"]["optional-dependencies"]["default"]
    required = next(p.removeprefix("yt-dlp-ejs==") for p in pins
                    if p.startswith("yt-dlp-ejs=="))
    text = FLAKE.read_text()
    current = EJS_URL.search(text).group(2)
    if required != current:
        FLAKE.write_text(EJS_URL.sub(rf"\g<1>{required}\g<3>", text, count=1))
        nix("flake", "lock")
        print(f"yt-dlp-ejs {current} -> {required}")


def check():
    yt_dlp, python = nix("build", ".#yt-dlp", ".#ytmusicapi-python",
                         "--no-link", "--print-out-paths").split()
    # An inherited dev-shell PYTHONPATH would shadow the new packages.
    env = {k: v for k, v in os.environ.items() if k != "PYTHONPATH"}

    result = subprocess.run([f"{yt_dlp}/bin/yt-dlp", *YTDLP_ARGS], env=env,
                            text=True, capture_output=True)
    info = json.loads(result.stdout) if result.returncode == 0 else {}
    warnings = [line for line in result.stderr.splitlines() if JS_FAILURES.search(line)]
    if not info.get("title") or not info.get("formats") or warnings:
        sys.exit(f"yt-dlp check failed:\n{result.stderr}")
    version = subprocess.run([f"{yt_dlp}/bin/yt-dlp", "--version"], env=env, text=True,
                             check=True, capture_output=True).stdout.strip()
    print(f"yt-dlp {version}: {info['title']!r}, {len(info['formats'])} formats")

    result = subprocess.run([f"{python}/bin/python3", "-c", YTMUSICAPI_CHECK], env=env,
                            text=True, capture_output=True)
    if result.returncode != 0:
        sys.exit(f"ytmusicapi check failed:\n{result.stderr}")
    print(f"ytmusicapi: get_artist returned {result.stdout.strip()!r}")


if __name__ == "__main__":
    if "--no-update" not in sys.argv[1:]:
        update()
    check()
    print("Restart the stack in a new shell: ./dev down && nix develop -c ./dev up -D")
