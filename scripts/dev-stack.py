"""Service commands for ./dev. Process Compose owns their process groups."""

import os
from pathlib import Path
import socket
import subprocess
import sys

from dotenv import load_dotenv


ROOT = Path(__file__).resolve().parent.parent


def path_setting(name, default):
    path = Path(os.environ.get(name, default)).expanduser()
    return path if path.is_absolute() else ROOT / path


ANIME = path_setting("NRS_ANIME_STORE_DIR", ROOT / "../nrs-anime-store-go")
MUSIC = path_setting("NRS_MUSICLIB_DIR", ROOT / "../musiclib-rs")
STATE = path_setting("NRS_STACK_STATE_DIR", ROOT / ".dev-stack")
DB = path_setting("NRS_DB_PATH", ROOT / "nrs-strip.db")
PORTS = {
    "nrs-app": os.environ.get("NRS_APP_PORT", "3737"),
    "anime-store": os.environ.get("NRS_ANIME_STORE_PORT", "3738"),
    "musiclib": os.environ.get("NRS_MUSIC_STORE_PORT", "4600"),
    "ytdlp": os.environ.get("NRS_YTDLP_PORT", "3000"),
    "ytmusicapi": os.environ.get("NRS_YTMUSICAPI_PORT", "9001"),
}


def preflight():
    for repo, manifest in [(ANIME, "go.mod"), (MUSIC, "Cargo.toml")]:
        if not (repo / manifest).is_file():
            sys.exit(f"Missing checkout: {repo} ({manifest}). See README.md.")
    if not DB.is_file():
        sys.exit(f"NRS database not found: {DB}. Set NRS_DB_PATH to an existing database.")
    occupied = []
    seen = set()
    for service, value in PORTS.items():
        try:
            port = int(value)
            if not 1 <= port <= 65535 or port in seen:
                raise ValueError
        except ValueError:
            sys.exit(f"Invalid or duplicate port for {service}: {value}")
        seen.add(port)
        with socket.socket() as listener:
            # Match the servers' reuse behavior so TIME_WAIT after ./dev down
            # does not look like a still-running listener on the next startup.
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                listener.bind(("127.0.0.1", port))
            except OSError as exc:
                occupied.append(f"  {service}: 127.0.0.1:{port}: {exc.strerror}")
    if occupied:
        sys.exit("Cannot start the stack:\n" + "\n".join(occupied)
                 + "\nStop existing services first, or select different ports (README.md)."
                 + "\nUse ./dev status or ./dev attach if this stack is already running.")
    print(f"Starting NRS at http://127.0.0.1:{PORTS['nrs-app']} (database: {DB})", flush=True)


def run(service):
    repo = ANIME if service == "anime-store" else ROOT if service == "nrs-app" else MUSIC
    os.chdir(repo)
    # Go does not load .env itself. Use the same parsing and inherited-env
    # precedence for every service, without evaluating credentials as shell code.
    load_dotenv(repo / ".env", override=False)
    profile = os.environ.get("NRS_BUILD_PROFILE", "release")
    cargo = ["cargo", "run", "--locked", "--profile", profile]

    if service == "anime-store":
        os.environ["ANIME_STORE_BIND"] = f"127.0.0.1:{PORTS[service]}"
        STATE.mkdir(parents=True, exist_ok=True)
        binary = str(STATE / "anime-store")
        subprocess.run(["go", "build", "-o", binary, "."], check=True)
        command = [binary]
    elif service == "ytmusicapi":
        os.environ["HOST"] = "127.0.0.1"
        os.environ["PORT"] = PORTS[service]
        command = ["python3", "-u", "src/bin/ytmusicapi_server.py"]
    elif service == "ytdlp":
        os.environ["PROVIDER_SERVER_ADDR"] = f"127.0.0.1:{PORTS[service]}"
        os.environ.setdefault("RUST_LOG", "info")
        command = cargo + ["--bin", "ytdlp_server"]
    elif service == "musiclib":
        os.environ["YTDLP_SERVER_URL"] = f"http://127.0.0.1:{PORTS['ytdlp']}"
        os.environ["YTMUSICAPI_SERVER_URL"] = f"http://127.0.0.1:{PORTS['ytmusicapi']}"
        # The player's shuffle mix reads NRS scores on demand.
        os.environ["NRS_APP_URL"] = f"http://127.0.0.1:{PORTS['nrs-app']}"
        # match.rhai loads the learned matcher through the ffi module; without
        # it soft-dedup still runs but scores every pair DISTINCT.
        command = cargo + ["--features", "ffi-system", "--bin", "server",
                           "--", "--listen", f"127.0.0.1:{PORTS[service]}"]
        if "NRS_MUSIC_STORE_DB" in os.environ:
            command += ["--db", str(path_setting("NRS_MUSIC_STORE_DB", ""))]
    elif service == "nrs-app":
        os.environ["NRS_ANIME_STORE_URL"] = f"http://127.0.0.1:{PORTS['anime-store']}"
        os.environ["NRS_MUSIC_STORE_URL"] = f"http://127.0.0.1:{PORTS['musiclib']}"
        command = cargo + ["-p", "nrs-app", "--", str(DB), PORTS[service]]
    else:
        sys.exit(f"Unknown service: {service}")
    os.execvp(command[0], command)


if __name__ == "__main__":
    if sys.argv[1] == "preflight":
        preflight()
    else:
        run(sys.argv[1])
