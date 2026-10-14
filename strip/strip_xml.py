#!/usr/bin/env python3
"""Rewrite nrs-impl's XML into the stripped format (`nrs-strip/`).

The stripped files keep only what NRS itself decides. Per entry:

- `<source>` is replaced by `<bind url="..."/>` from `strip/bindings.tsv`.
- `title` moves into a `<!-- title -->` comment when a store can supply the
  title (musiclib or the anime store); otherwise it stays an attribute,
  since nothing else would know it.
- Containment the music store already asserts is removed: a nested entry
  whose album -> track edge musiclib has is hoisted to the top level, and an
  explicit `<contains id=.../>` with factor 1 for such an edge is dropped.
  The importer re-derives exactly these edges (same rule: both ends map
  one-to-one onto musiclib entries, and musiclib has the child row).

- Progress numbers the stores reproduce are removed (the importer refills
  them, `crates/nrs-import/src/stores.rs`): a `<musicConsumedProgress>`
  `length` within 10 s of the bound track's musiclib length (except
  `generatedBy="user"` lengths, which are deliberate cuts), an
  `<animeConsumedProgress>` `episodes` equal to the anime store's count, and
  an `episodeDuration` within a minute of what the importer would use (the
  store's length for movies and shorts, 20 minutes for TV episodes).

Everything else (impacts, roles, remix/featureMusic/killedBy, defs, scripts,
includes, comments) is copied as is. Nested entries under an entry with a
direct `<script>`/`<ref>` child stay nested, because entry-level `local`
script state flows into child scopes.

Usage: strip_xml.py [SRC_DIR] [OUT_DIR]   (defaults: ../nrs-impl/nrs,
../nrs-impl/nrs-strip). Prints a report of what changed.
"""

from __future__ import annotations

import csv
import shutil
import sqlite3
import sys
import xml.etree.ElementTree as ET
from collections import Counter, defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
BINDINGS = HERE / "bindings.tsv"
MUSICLIB_DB = ROOT / "musiclib.db"
ANIME_DB = ROOT.parent / "nrs-anime-store-go" / "anime.db"
MUSIC_LENGTH_SLACK_S = 10
EPISODE_LENGTH_SLACK_S = 60
DEFAULT_EPISODE_S = 20 * 60


def load_bindings() -> dict[str, dict[str, str]]:
    with BINDINGS.open(newline="", encoding="utf-8") as handle:
        return {row["entry_id"]: row for row in csv.DictReader(handle, delimiter="\t")}


class StoreEdges:
    """Album -> track edges musiclib asserts between NRS entries.

    Only `release` -> `track` rows count: `entry_child` is musiclib's fetch
    graph and also holds track -> release and release -> artist edges.

    An NRS entry takes part only if its bind URL maps to a musiclib entry
    that no other NRS entry maps to (the one-to-one rule the importer uses).
    """

    def __init__(self, bindings: dict[str, dict[str, str]]):
        conn = sqlite3.connect(f"file:{MUSICLIB_DB}?mode=ro", uri=True)
        lib_entry: dict[str, int] = {}
        for entry_id, row in bindings.items():
            if row["title_store"] != "musiclib":
                continue
            found = conn.execute(
                "SELECT entry_id FROM entry_source WHERE identifier = ? LIMIT 1", (row["url"],)
            ).fetchone()
            if found:
                lib_entry[entry_id] = found[0]
        per_lib: dict[int, list[str]] = defaultdict(list)
        for entry_id, lib_id in lib_entry.items():
            per_lib[lib_id].append(entry_id)
        self.nrs_of = {lib_id: ids[0] for lib_id, ids in per_lib.items() if len(ids) == 1}
        self.lib_of = {nrs: lib for lib, nrs in self.nrs_of.items()}
        self.ambiguous = sorted(e for ids in per_lib.values() if len(ids) > 1 for e in ids)
        self.edges: set[tuple[str, str]] = set()
        for lib_parent, nrs_parent in self.nrs_of.items():
            for (lib_child,) in conn.execute(
                "SELECT DISTINCT c.entry_id FROM entry_source p "
                "JOIN entry pe ON pe.id = p.entry_id "
                "JOIN entry_child ch ON ch.parent_source = p.source AND ch.parent_identifier = p.identifier "
                "JOIN entry_source c ON c.source = ch.child_source AND c.identifier = ch.child_identifier "
                "JOIN entry ce ON ce.id = c.entry_id "
                "WHERE p.entry_id = ? AND pe.entry_type = 'release' AND ce.entry_type = 'track'",
                (lib_parent,),
            ):
                if lib_child in self.nrs_of:
                    self.edges.add((nrs_parent, self.nrs_of[lib_child]))

    def has(self, parent: str, child: str) -> bool:
        return (parent, child) in self.edges


def legacy_edges(root: ET.Element) -> set[tuple[str, str]]:
    """Contains edges the legacy XML declares: nested entries and explicit
    `<contains>` (including grouped `<contains><contains id/></contains>`)."""
    found: set[tuple[str, str]] = set()

    def contains_ids(node: ET.Element, enclosing: str):
        if node.get("id"):
            found.add((enclosing, node.get("id").replace("$", enclosing)))
        for child in node:
            if child.tag == "contains":
                contains_ids(child, enclosing)

    def walk(node: ET.Element, enclosing: str | None):
        for child in node:
            if child.tag == "def":
                continue
            if child.tag == "entry":
                raw = child.get("id", "")
                full = raw.replace("$", enclosing) if enclosing else raw
                if enclosing:
                    found.add((enclosing, full))
                walk(child, full)
            elif child.tag == "contains" and enclosing:
                contains_ids(child, enclosing)

    walk(root, None)
    return found


def seconds(value: str) -> int:
    total = 0
    for part in value.split(":"):
        total = total * 60 + int(part)
    return total


class StoreProgress:
    """What the importer fills into progress impacts that leave it out."""

    def __init__(self):
        self.music = sqlite3.connect(f"file:{MUSICLIB_DB}?mode=ro", uri=True)
        self.anime = sqlite3.connect(f"file:{ANIME_DB}?mode=ro", uri=True)

    def track_seconds(self, url: str) -> float | None:
        row = self.music.execute(
            "SELECT s.duration_ms FROM entry_source b JOIN entry_source s ON s.entry_id = b.entry_id "
            "WHERE b.identifier = ?1 AND s.duration_ms > 0 "
            "ORDER BY (s.identifier = ?1) DESC, "
            "CASE s.source WHEN 'musicbrainz' THEN 0 WHEN 'spotify' THEN 1 ELSE 2 END, "
            "s.source, s.identifier LIMIT 1",
            (url,),
        ).fetchone()
        return row[0] / 1000 if row else None

    def anime_basis(self, url: str) -> tuple[int | None, int | None]:
        parts = url.rstrip("/").split("/")
        if "myanimelist.net" not in url or len(parts) < 5:
            return None, None
        row = self.anime.execute(
            "SELECT json_extract(e.extra, '$.progress_basis.episode_count'), "
            "json_extract(e.extra, '$.progress_basis.episode_duration_seconds') "
            "FROM external_id x JOIN entry e ON e.id = x.entry_id WHERE x.provider_id = ?",
            (f"mal:{parts[3]}:{parts[4]}",),
        ).fetchone()
        return (row[0], row[1]) if row else (None, None)

    @staticmethod
    def filled_episode_seconds(store_seconds: int | None) -> int:
        # stores::is_tv_episode: 15..30 minutes counts as a TV episode.
        if store_seconds and not 15 * 60 <= store_seconds < 30 * 60:
            return store_seconds
        return DEFAULT_EPISODE_S


def parse(path: Path) -> tuple[str, ET.Element]:
    text = path.read_text(encoding="utf-8")
    head = text[: text.index("<document")]
    parser = ET.XMLParser(target=ET.TreeBuilder(insert_comments=True, insert_pis=True))
    return head, ET.fromstring(text, parser=parser)


def fraction(value: str | None) -> float:
    if value is None:
        return 1.0
    if "/" in value:
        num, den = value.split("/", 1)
        return float(num) / float(den)
    return float(value)


class Stripper:
    def __init__(self, bindings, edges: StoreEdges, unlinks: dict[str, list[str]],
                 progress: StoreProgress):
        self.bindings = bindings
        self.progress = progress
        self.edges = edges
        # parent -> children whose store-derived edge the legacy XML never
        # declared; written once as <unlink id=".."/> so scores don't move.
        self.unlinks = {k: sorted(v) for k, v in unlinks.items()}
        self.report: Counter = Counter()
        self.kept_nested_for_scripts: list[str] = []
        self.unbound_titled: list[str] = []

    def full_id(self, raw: str, enclosing: str | None) -> str:
        return raw.replace("$", enclosing) if enclosing else raw

    def strip_children(self, parent: ET.Element, enclosing: str | None, scripted: bool,
                       hoisted: list[ET.Element]) -> None:
        """Process `parent`'s children in place. `hoisted` collects nested
        entries moved to the document level (in order)."""
        new_children: list[ET.Element] = []
        for child in list(parent):
            if child.tag == "def":
                new_children.append(child)
                continue
            if child.tag == "entry" and enclosing is None:
                # Hoisted descendants go right after their top-level
                # ancestor, so document order (which scripts observe)
                # moves as little as possible.
                local: list[ET.Element] = []
                new_children.extend(self.strip_entry(child, enclosing, scripted, local, parent))
                new_children.extend(local)
                continue
            if child.tag == "entry":
                new_children.extend(self.strip_entry(child, enclosing, scripted, hoisted, parent))
                continue
            if child.tag == "contains" and enclosing and self.drop_contains(child, enclosing):
                continue
            new_children.append(child)
        tail = parent.text
        parent[:] = new_children
        parent.text = tail

    def drop_contains(self, node: ET.Element, enclosing: str) -> bool:
        target = node.get("id")
        if target is None or len(node) or fraction(node.get("factor")) != 1.0:
            return False
        if self.edges.has(enclosing, self.full_id(target, enclosing)):
            self.report["explicit <contains> dropped (store-derived)"] += 1
            return True
        return False

    def strip_entry(self, node: ET.Element, enclosing: str | None, scripted: bool,
                    hoisted: list[ET.Element], container: ET.Element) -> list[ET.Element]:
        raw_id = node.get("id", "")
        entry_id = self.full_id(raw_id, enclosing)
        binding = self.bindings.get(entry_id)
        out: list[ET.Element] = []

        source = node.find("source")
        if source is not None:
            node.remove(source)
            self.report["<source> removed"] += 1
        if binding and binding["url"]:
            bind = ET.Element("bind", {"url": binding["url"]})
            node.insert(0, bind)
            self.report["<bind> added"] += 1
        for position, target in enumerate(self.unlinks.pop(entry_id, []), start=1):
            node.insert(position, ET.Element("unlink", {"id": target}))
            self.report["<unlink> added (store edge not in legacy XML)"] += 1

        title = node.get("title")
        if binding and binding["title_store"] and title is not None:
            del node.attrib["title"]
            out.append(ET.Comment(f" {title} "))
            self.report["title -> comment"] += 1
            # The legacy importer turns "feat."/"ft." in a title into the
            # featuring role rule. Titles are store-owned now, so pin it.
            if "feat." in title or "ft." in title:
                musicvar = node.find("musicvar")
                if musicvar is None:
                    at = 1 + sum(1 for c in node if c.tag in ("bind", "unlink"))
                    node.insert(at, ET.Element("musicvar", {"feat": "true"}))
                    self.report["<musicvar feat> added (title had feat.)"] += 1
                elif musicvar.get("feat") is None:
                    musicvar.set("feat", "true")
                    self.report["<musicvar feat> added (title had feat.)"] += 1
        elif title is not None:
            self.report["title kept (no store)"] += 1
            if not (binding and binding["url"]):
                self.unbound_titled.append(entry_id)

        if binding and binding["url"]:
            self.strip_progress(node, binding["url"])

        has_script = any(c.tag in ("script", "ref") for c in node)
        self.strip_children(node, entry_id, scripted or has_script, hoisted)

        # Hoist this entry if its containment is store-derived.
        if enclosing and container.tag == "entry" and self.edges.has(enclosing, entry_id):
            if scripted or any(d.tag in ("script", "ref") for d in node.iter()):
                self.kept_nested_for_scripts.append(entry_id)
                self.report["nested kept (enclosing script scope)"] += 1
            else:
                node.set("id", entry_id)
                hoisted.extend(out + [node])
                self.report["nested entry hoisted (store-derived)"] += 1
                return []
        elif enclosing and container.tag == "entry":
            self.report["nested kept (not store-derived)"] += 1
        return out + [node]

    def strip_progress(self, node: ET.Element, url: str) -> None:
        for impact in node:
            if impact.tag == "musicConsumedProgress":
                length = impact.get("length")
                if length is None or impact.get("generatedBy") == "user":
                    continue
                store = self.progress.track_seconds(url)
                if store is not None and abs(store - seconds(length)) <= MUSIC_LENGTH_SLACK_S:
                    del impact.attrib["length"]
                    impact.attrib.pop("generatedBy", None)
                    self.report["progress: music length -> musiclib"] += 1
                else:
                    self.report["progress: music length kept"] += 1
            elif impact.tag == "animeConsumedProgress":
                count, store_seconds = self.progress.anime_basis(url)
                episodes = impact.get("episodes")
                if episodes is not None and count is not None and float(episodes) == count:
                    del impact.attrib["episodes"]
                    self.report["progress: episodes -> anime store"] += 1
                elif episodes is not None:
                    self.report["progress: episodes kept"] += 1
                duration = impact.get("episodeDuration")
                if duration is None:
                    continue
                filled = self.progress.filled_episode_seconds(store_seconds)
                if abs(filled - seconds(duration)) <= EPISODE_LENGTH_SLACK_S:
                    del impact.attrib["episodeDuration"]
                    self.report["progress: episodeDuration -> anime store"] += 1
                else:
                    self.report["progress: episodeDuration kept"] += 1

    def strip_document(self, root: ET.Element) -> None:
        self.strip_children(root, None, False, [])


def main() -> None:
    src = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT.parent / "nrs-impl" / "nrs"
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else ROOT.parent / "nrs-impl" / "nrs-strip"
    bindings = load_bindings()
    edges = StoreEdges(bindings)
    declared: set[tuple[str, str]] = set()
    for path in sorted(src.rglob("*.xml")):
        declared |= legacy_edges(parse(path)[1])
    unlinks: dict[str, list[str]] = defaultdict(list)
    for parent, child in edges.edges - declared:
        unlinks[parent].append(child)
    stripper = Stripper(bindings, edges, unlinks, StoreProgress())

    if out.exists():
        shutil.rmtree(out)
    for path in sorted(src.rglob("*")):
        target = out / path.relative_to(src)
        if path.is_dir():
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        if path.suffix != ".xml":
            shutil.copy2(path, target)
            continue
        head, root = parse(path)
        stripper.strip_document(root)
        ET.indent(root, space="    ")
        body = ET.tostring(root, encoding="unicode")
        target.write_text(head + body + "\n", encoding="utf-8")

    print(f"wrote {out}")
    print(f"store-derived contains edges between NRS entries: {len(edges.edges)}")
    print(f"store edges not declared in the legacy XML (unlinked): {sum(map(len, unlinks.values()))}")
    if stripper.unlinks:
        print("WARNING: unlinks for entries never declared:", stripper.unlinks)
    print(f"entries excluded (bound to a musiclib entry shared with another NRS entry): {len(edges.ambiguous)}")
    for key, value in sorted(stripper.report.items()):
        print(f"  {key}: {value}")
    if stripper.kept_nested_for_scripts:
        print("nested kept because an enclosing entry runs scripts:",
              " ".join(stripper.kept_nested_for_scripts[:20]),
              "..." if len(stripper.kept_nested_for_scripts) > 20 else "")


if __name__ == "__main__":
    main()
