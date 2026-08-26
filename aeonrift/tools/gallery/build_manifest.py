"""Build the champion gallery manifest from the roster + content.

Combines, for every champion .glb under assets/champions/<slug>/model.glb:
  - display name  (content/champions/<slug>.ron  ->  name: "...")
  - archetype + faction  (content/champions/roster_meta.ron sidecar)
  - a faction tint color (for the greybox render + the HTML grid)

Writes screenshots/champions_manifest.json. Plain stdlib, no Blender.
"""

import json
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CHAMP_DIR = os.path.join(ROOT, "assets", "champions")
CONTENT = os.path.join(ROOT, "content", "champions")
ROSTER = os.path.join(CONTENT, "roster_meta.ron")
OUT = os.path.join(ROOT, "screenshots", "champions_manifest.json")

FACTION_COLOR = {
    "Greek": "#d4af37",     # gold
    "Norse": "#5b8fb0",     # steel-blue
    "Roman": "#c1272d",     # crimson
    "Egyptian": "#d9a441",  # sand-gold
    "Ember": "#ff6b35",     # orange
    "Tide": "#22d3ee",      # cyan
    "Verdant": "#43a047",   # green
    "Umbral": "#7c4dff",    # violet
}
DEFAULT_COLOR = "#8a8a92"


def parse_roster_meta(path):
    meta = {}
    if not os.path.exists(path):
        return meta
    txt = open(path, encoding="utf-8").read()
    # "slug": (archetype: "x", faction: "y"),
    pat = re.compile(
        r'"([a-z0-9\-]+)"\s*:\s*\(\s*archetype:\s*"([a-z]+)"\s*,\s*faction:\s*"([A-Za-z]+)"\s*\)'
    )
    for m in pat.finditer(txt):
        meta[m.group(1)] = (m.group(2), m.group(3))
    return meta


def display_name(slug):
    ron = os.path.join(CONTENT, slug + ".ron")
    if os.path.exists(ron):
        txt = open(ron, encoding="utf-8").read()
        m = re.search(r'name:\s*"([^"]+)"', txt)
        if m:
            return m.group(1)
    return " ".join(w.capitalize() for w in slug.split("-"))


def main():
    meta = parse_roster_meta(ROSTER)
    rows = []
    slugs = sorted(
        d for d in os.listdir(CHAMP_DIR)
        if os.path.isfile(os.path.join(CHAMP_DIR, d, "model.glb"))
    )
    for slug in slugs:
        arche, faction = meta.get(slug, ("unknown", "Unknown"))
        rows.append({
            "slug": slug,
            "name": display_name(slug),
            "archetype": arche,
            "faction": faction,
            "color": FACTION_COLOR.get(faction, DEFAULT_COLOR),
            "glb": os.path.join("assets", "champions", slug, "model.glb"),
            "png": os.path.join("screenshots", "champions", slug + ".png"),
        })
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    json.dump(rows, open(OUT, "w", encoding="utf-8"), indent=2)
    print("manifest: %d champions -> %s" % (len(rows), OUT))
    # tiny sanity summary
    by_faction = {}
    for r in rows:
        by_faction[r["faction"]] = by_faction.get(r["faction"], 0) + 1
    print("by faction:", by_faction)


if __name__ == "__main__":
    main()
