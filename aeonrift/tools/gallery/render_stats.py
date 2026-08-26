"""Render a clean project stats infographic (SVG -> PNG).

Summarizes the shipped content: 47 champions by faction, the content catalog
counts, and the workspace crate count. All numbers are counted live from the
repo so the card can't drift from reality.
"""

import json
import os
import re

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CONTENT = os.path.join(ROOT, "content")
MANIFEST = os.path.join(ROOT, "screenshots", "champions_manifest.json")
OUT_SVG = os.path.join(ROOT, "screenshots", "misc", "stats-infographic.svg")

W, H = 1400, 900

FACTION_COLOR = {
    "Greek": "#d4af37", "Norse": "#5b8fb0", "Roman": "#c1272d", "Egyptian": "#d9a441",
    "Ember": "#ff6b35", "Tide": "#22d3ee", "Verdant": "#43a047", "Umbral": "#7c4dff",
}


def count_ron(sub):
    d = os.path.join(CONTENT, sub)
    if not os.path.isdir(d):
        return 0
    n = 0
    for f in os.listdir(d):
        if f.endswith(".ron") and not re.search(r"schema|readme|roster_meta", f, re.I):
            n += 1
    return n


def count_crates():
    n = 0
    for dp, _, files in os.walk(ROOT):
        if "/target" in dp:
            continue
        if "Cargo.toml" in files and os.path.abspath(dp) != ROOT:
            n += 1
    return n


def esc(s):
    return str(s).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def main():
    champs = json.load(open(MANIFEST, encoding="utf-8"))
    by_faction = {}
    for c in champs:
        by_faction[c["faction"]] = by_faction.get(c["faction"], 0) + 1

    catalog = [
        ("Items", count_ron("items")),
        ("Runes", count_ron("runes")),
        ("Champions", count_ron("champions")),
        ("Buffs", count_ron("buffs")),
        ("Status effects", count_ron("status_effects")),
        ("Monsters", count_ron("monsters")),
        ("Summoner spells", count_ron("summoner_spells")),
        ("Objectives", count_ron("objectives")),
        ("Maps", count_ron("maps")),
        ("Minions", count_ron("minions")),
        ("Turrets", count_ron("turrets")),
    ]
    total_content = sum(n for _, n in catalog)
    crates = count_crates()

    s = []
    s.append(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="Inter, Segoe UI, sans-serif">')
    s.append(f'<rect width="{W}" height="{H}" fill="#0e1116"/>')
    s.append(f'<text x="50" y="60" fill="#e8edf4" font-size="34" font-weight="800">Aeonrift — project at a glance</text>')
    s.append(f'<text x="50" y="90" fill="#7f8ea3" font-size="17">A Rust MOBA monorepo: authored content, a deterministic sim core, and a 3D asset pipeline (greybox).</text>')

    # Big stat tiles
    tiles = [
        (len(champs), "champions", "#d4af37"),
        (crates, "workspace crates", "#3da5ff"),
        (len(by_faction), "factions", "#7c4dff"),
        (total_content, "content entries", "#43a047"),
    ]
    tx, ty, tw, gap = 50, 120, 315, 20
    for i, (num, label, col) in enumerate(tiles):
        x = tx + i * (tw + gap)
        s.append(f'<rect x="{x}" y="{ty}" width="{tw}" height="130" rx="14" fill="#161d27" stroke="{col}" stroke-width="2"/>')
        s.append(f'<text x="{x+24}" y="{ty+78}" fill="{col}" font-size="60" font-weight="800">{num}</text>')
        s.append(f'<text x="{x+26}" y="{ty+110}" fill="#cdd6e2" font-size="18">{label}</text>')

    # Faction breakdown (horizontal bars)
    fy = 300
    s.append(f'<text x="50" y="{fy}" fill="#e8edf4" font-size="22" font-weight="700">Champions by faction ({len(champs)})</text>')
    order = sorted(by_faction.items(), key=lambda kv: (-kv[1], kv[0]))
    maxv = max(by_faction.values()) if by_faction else 1
    bx, bw = 230, 380
    row_h = 42
    for i, (fac, n) in enumerate(order):
        y = fy + 30 + i * row_h
        col = FACTION_COLOR.get(fac, "#8a8a92")
        s.append(f'<text x="50" y="{y+20}" fill="#cdd6e2" font-size="17">{esc(fac)}</text>')
        s.append(f'<rect x="{bx}" y="{y+2}" width="{bw}" height="24" rx="6" fill="#1b2430"/>')
        w = bw * (n / maxv)
        s.append(f'<rect x="{bx}" y="{y+2}" width="{w:.0f}" height="24" rx="6" fill="{col}"/>')
        s.append(f'<text x="{bx+w+10:.0f}" y="{y+20}" fill="#e8edf4" font-size="16" font-weight="700">{n}</text>')

    # Content catalog (right column)
    cx = 760
    s.append(f'<text x="{cx}" y="{fy}" fill="#e8edf4" font-size="22" font-weight="700">Authored content catalog ({total_content})</text>')
    cmax = max(n for _, n in catalog)
    cbx, cbw = cx + 180, 350
    for i, (label, n) in enumerate(catalog):
        y = fy + 30 + i * 42
        s.append(f'<text x="{cx}" y="{y+18}" fill="#cdd6e2" font-size="16">{esc(label)}</text>')
        s.append(f'<rect x="{cbx}" y="{y+2}" width="{cbw}" height="22" rx="6" fill="#1b2430"/>')
        w = cbw * (n / cmax)
        s.append(f'<rect x="{cbx}" y="{y+2}" width="{w:.0f}" height="22" rx="6" fill="#3da5ff"/>')
        s.append(f'<text x="{cbx+w+10:.0f}" y="{y+19}" fill="#e8edf4" font-size="15" font-weight="700">{n}</text>')

    s.append(f'<text x="50" y="{H-24}" fill="#5c6a7e" font-size="14">Counts read live from content/*.ron and the workspace Cargo manifests · champion models are greybox 3D (final art pending).</text>')
    s.append('</svg>')
    os.makedirs(os.path.dirname(OUT_SVG), exist_ok=True)
    open(OUT_SVG, "w", encoding="utf-8").write("\n".join(s))
    print("stats svg -> %s  (champions=%d crates=%d content=%d factions=%d)" % (
        OUT_SVG, len(champs), crates, total_content, len(by_faction)))


if __name__ == "__main__":
    main()
