#!/usr/bin/env python3
"""Build the entry -> canonical URL binding table for the stripped XML.

One bind URL per NRS entry, chosen from the migration decisions (latest
wins) and, for non-music entries, from the legacy store ids. URLs that
musiclib knows are rewritten to musiclib's stored spelling, so the
importer can look them up verbatim.

Writes strip/bindings.tsv: entry_id, url, origin, title_store
(`musiclib`, `anime`, or empty when no store can supply a title).
"""

from __future__ import annotations

import csv
import re
import sqlite3
import sys
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LEDGERS = ROOT / "result-vgmdb-musicbrainz"
NRS_DB = ROOT / "nrs.db"
MUSICLIB_DB = ROOT / "musiclib.db"
OUT = Path(__file__).resolve().parent / "bindings.tsv"


def read_tsv(path: Path) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle, delimiter="\t"))


def musiclib_lookup(conn: sqlite3.Connection, url: str) -> str | None:
    """musiclib's stored spelling of `url`, if it has the record."""
    candidates = [url]
    youtube = re.search(r"(?:youtube\.com/watch\?v=|youtu\.be/)([\w-]{11})", url)
    if youtube:
        candidates.append(f"https://youtu.be/{youtube.group(1)}")
    niconico = re.search(r"nicovideo\.jp/watch/([a-z]{2}\d+)", url)
    if niconico:
        vid = niconico.group(1)
        candidates += [f"https://www.nicovideo.jp/watch/{vid}", f"http://www.nicovideo.jp/watch/{vid}"]
    for candidate in candidates:
        if conn.execute(
            "SELECT 1 FROM entry_source WHERE identifier = ? LIMIT 1", (candidate,)
        ).fetchone():
            return candidate
    return None


def main() -> None:
    nrs = sqlite3.connect(f"file:{NRS_DB}?mode=ro", uri=True)
    lib = sqlite3.connect(f"file:{MUSICLIB_DB}?mode=ro", uri=True)
    entries = {
        row[0]: (row[1], row[2])
        for row in nrs.execute("SELECT id, store, store_id FROM entry")
    }

    # entry_id -> (url, origin); later sources overwrite earlier ones.
    chosen: dict[str, tuple[str, str]] = {}
    local_only: set[str] = set()

    for row in read_tsv(LEDGERS / "migration-ledger.tsv"):
        url = row["musicbrainz_url"] or row["vgmdb_url"]
        if url:
            chosen[row["legacy_id"]] = (url, "migration-ledger")
    for name, key in (
        ("timestamp-album-decisions.applied.tsv", "legacy_album_id"),
        ("timestamp-artist-decisions.applied.tsv", "legacy_artist_id"),
        ("timestamp-track-decisions.applied.tsv", "legacy_track_id"),
    ):
        for row in read_tsv(LEDGERS / name):
            if row["choice"]:
                chosen[row[key]] = (row["choice"], name.split(".")[0])
            else:
                chosen.pop(row[key], None)
                local_only.add(row[key])
    for row in read_tsv(LEDGERS / "final-stragglers-decisions.tsv"):
        # An "equivalent album" pick only brought the songs into musiclib;
        # it is a different release, so it is not the entry's identity.
        if row["note"].startswith("equivalent album"):
            chosen[row["legacy_id"]] = (row["current_url"], "final-stragglers (equivalent album not bound)")
        elif row["choice"]:
            chosen[row["legacy_id"]] = (row["choice"], "final-stragglers")

    for entry_id, (store, store_id) in entries.items():
        if entry_id in chosen or entry_id in local_only:
            continue
        prefix = entry_id.split("-")[0]
        number = entry_id.rsplit("-", 1)[-1]
        if store == "mal" and prefix == "A":
            chosen[entry_id] = (f"https://myanimelist.net/anime/{store_id}", "nrs.db mal")
        elif re.fullmatch(r"V-VNDB-\d+", entry_id):
            chosen[entry_id] = (f"https://vndb.org/v{number}", "legacy id")
        elif re.fullmatch(r"L-MAL-\d+", entry_id):
            chosen[entry_id] = (f"https://myanimelist.net/manga/{number}", "legacy id")
        elif re.fullmatch(r"(F|G|GF)-VGMDB-\d+", entry_id):
            chosen[entry_id] = (f"https://vgmdb.net/product/{number}", "legacy id")
        elif store.startswith("url-") and store_id.startswith("http"):
            chosen[entry_id] = (store_id, f"nrs.db {store}")

    rows = []
    for entry_id in sorted(entries):
        if entry_id not in chosen:
            rows.append((entry_id, "", "local", ""))
            continue
        url, origin = chosen[entry_id]
        stored = musiclib_lookup(lib, url)
        if stored:
            rows.append((entry_id, stored, origin, "musiclib"))
        elif re.match(r"https://(myanimelist\.net/(anime|manga)|anilist\.co/(anime|manga)|vndb\.org)/", url):
            rows.append((entry_id, url, origin, "anime"))
        else:
            rows.append((entry_id, url, origin, ""))

    with OUT.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n")
        writer.writerow(["entry_id", "url", "origin", "title_store"])
        writer.writerows(rows)

    by_url: dict[str, list[str]] = defaultdict(list)
    for entry_id, url, _, _ in rows:
        if url:
            by_url[url].append(entry_id)
    dups = {u: ids for u, ids in by_url.items() if len(ids) > 1}
    print(f"entries {len(rows)}", file=sys.stderr)
    print("title store:", dict(Counter(r[3] or ("no bind" if not r[1] else "none") for r in rows)), file=sys.stderr)
    print("by prefix/title store:", dict(Counter((r[0].split('-')[0], r[3] or ("no bind" if not r[1] else "none")) for r in rows)), file=sys.stderr)
    print(f"URLs bound to more than one entry: {len(dups)}", file=sys.stderr)
    for url, ids in sorted(dups.items()):
        print(f"  {url}  {' '.join(ids)}", file=sys.stderr)


if __name__ == "__main__":
    main()
