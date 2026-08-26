"""Render the repo crate-architecture graph as an SVG (then PNG).

graphviz `dot` is not installed in this environment, so instead of a hairball of
all 829 knowledge-graph edges we render the *architecture* view by hand: the
workspace crates, clustered by top-level directory (engine / games / tools), with
the real DEPENDS_ON edges (parsed from the graph tool's DOT export) drawn as
arrows. The full DOT + Mermaid exports are saved alongside for the complete graph.
"""

import math
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
DOT = os.path.join(ROOT, "screenshots", "misc", "architecture-graph.dot")
OUT_SVG = os.path.join(ROOT, "screenshots", "misc", "architecture-graph.svg")

W, H = 1500, 1000

LAYER_STYLE = {
    "engine": ("#12324a", "#2f7fb0", "#bfe3ff"),   # panel, border, chip-text
    "games":  ("#3a2140", "#a05bbf", "#f0d6ff"),
    "tools":  ("#123a2a", "#3fa079", "#c8f5df"),
}


def crate_paths():
    """Map crate name -> top-level dir by scanning Cargo.toml files."""
    out = {}
    for dirpath, _, files in os.walk(ROOT):
        if "/target" in dirpath:
            continue
        if "Cargo.toml" in files and os.path.abspath(dirpath) != ROOT:
            txt = open(os.path.join(dirpath, "Cargo.toml"), encoding="utf-8", errors="ignore").read()
            m = re.search(r'(?m)^\s*name\s*=\s*"([^"]+)"', txt)
            if not m:
                continue
            rel = os.path.relpath(dirpath, ROOT)
            out[m.group(1)] = rel.split(os.sep)[0]
    return out


def depends_edges():
    edges = []
    if not os.path.exists(DOT):
        return edges
    for m in re.finditer(r'"Crate:([^"]+)"\s*->\s*"Crate:([^"]+)"\s*\[label="DEPENDS_ON"\]', open(DOT, encoding="utf-8").read()):
        edges.append((m.group(1), m.group(2)))
    return edges


def grid(items, x, y, w, h, cols, chip_w, chip_h, gap_x, gap_y):
    pos = {}
    for i, name in enumerate(items):
        r, c = divmod(i, cols)
        cx = x + c * (chip_w + gap_x)
        cy = y + r * (chip_h + gap_y)
        pos[name] = (cx, cy, chip_w, chip_h)
    return pos


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def short(name):
    return name.replace("aeonrift-", "ar·").replace("league_", "lg_").replace("lol_", "lol_")


def main():
    paths = crate_paths()
    edges = [(a, b) for a, b in depends_edges() if a in paths and b in paths]

    layers = {"engine": [], "games": [], "tools": []}
    for name, layer in sorted(paths.items()):
        layers.get(layer, layers["engine"]).append(name)

    s = []
    s.append(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="Inter, Segoe UI, sans-serif">')
    s.append(f'<rect width="{W}" height="{H}" fill="#0e1116"/>')
    s.append('<defs><marker id="arrow" markerWidth="9" markerHeight="9" refX="7" refY="3" orient="auto" markerUnits="strokeWidth"><path d="M0,0 L7,3 L0,6 z" fill="#e8b04b"/></marker></defs>')
    s.append(f'<text x="40" y="46" fill="#e8edf4" font-size="28" font-weight="800">Aeonrift — crate architecture</text>')
    s.append(f'<text x="40" y="74" fill="#7f8ea3" font-size="16">{len(paths)} workspace crates, clustered by directory · arrows = DEPENDS_ON (from the knowledge-graph export)</text>')

    chip_h = 30
    pos = {}

    # Engine panel (left, big) — the foundation.
    ex, ey, ew, eh = 40, 100, 880, 860
    s.append(f'<rect x="{ex}" y="{ey}" width="{ew}" height="{eh}" rx="14" fill="{LAYER_STYLE["engine"][0]}" fill-opacity="0.35" stroke="{LAYER_STYLE["engine"][1]}" stroke-width="1.5"/>')
    s.append(f'<text x="{ex+18}" y="{ey+30}" fill="{LAYER_STYLE["engine"][2]}" font-size="18" font-weight="700">engine/  ({len(layers["engine"])})</text>')
    pos.update(grid(layers["engine"], ex + 20, ey + 52, ew - 40, eh - 70, 3, 270, chip_h, 12, 12))

    # Games panel (top-right).
    gx, gy, gw, gh = 960, 100, 500, 300
    s.append(f'<rect x="{gx}" y="{gy}" width="{gw}" height="{gh}" rx="14" fill="{LAYER_STYLE["games"][0]}" fill-opacity="0.4" stroke="{LAYER_STYLE["games"][1]}" stroke-width="1.5"/>')
    s.append(f'<text x="{gx+18}" y="{gy+30}" fill="{LAYER_STYLE["games"][2]}" font-size="18" font-weight="700">games/  ({len(layers["games"])})</text>')
    pos.update(grid(layers["games"], gx + 20, gy + 52, gw - 40, gh - 70, 1, gw - 60, chip_h, 12, 14))

    # Tools panel (bottom-right).
    tx, ty, tw, th = 960, 440, 500, 520
    s.append(f'<rect x="{tx}" y="{ty}" width="{tw}" height="{th}" rx="14" fill="{LAYER_STYLE["tools"][0]}" fill-opacity="0.4" stroke="{LAYER_STYLE["tools"][1]}" stroke-width="1.5"/>')
    s.append(f'<text x="{tx+18}" y="{ty+30}" fill="{LAYER_STYLE["tools"][2]}" font-size="18" font-weight="700">tools/  ({len(layers["tools"])})</text>')
    pos.update(grid(layers["tools"], tx + 20, ty + 52, tw - 40, th - 70, 1, tw - 60, chip_h, 12, 14))

    # Chips
    for name, layer in sorted(paths.items()):
        if name not in pos:
            continue
        x, y, w, h = pos[name]
        style = LAYER_STYLE.get(layer, LAYER_STYLE["engine"])
        involved = any(name in (a, b) for a, b in edges)
        fill = "#1b2430" if not involved else "#243347"
        stroke = style[1] if not involved else "#e8b04b"
        s.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="7" fill="{fill}" stroke="{stroke}" stroke-width="{2 if involved else 1}"/>')
        s.append(f'<text x="{x+10}" y="{y+20}" fill="{style[2]}" font-size="14" font-weight="500">{esc(name)}</text>')

    # Edges (draw as gentle curves between chip anchors)
    def anchor(name):
        x, y, w, h = pos[name]
        return x + w / 2, y + h / 2

    for a, b in edges:
        if a not in pos or b not in pos:
            continue
        ax, ay = anchor(a)
        bx, by = anchor(b)
        # anchor to nearest vertical edge of the chips to reduce overlap
        mx = (ax + bx) / 2
        s.append(f'<path d="M {ax:.0f} {ay:.0f} C {mx:.0f} {ay:.0f} {mx:.0f} {by:.0f} {bx:.0f} {by:.0f}" fill="none" stroke="#e8b04b" stroke-width="1.4" opacity="0.5" marker-end="url(#arrow)"/>')

    s.append(f'<text x="40" y="{H-16}" fill="#5c6a7e" font-size="13">Full knowledge graph (crates + champions + abilities + assets, 800+ edges) exported to architecture-graph.dot / .mermaid.</text>')
    s.append('</svg>')
    os.makedirs(os.path.dirname(OUT_SVG), exist_ok=True)
    open(OUT_SVG, "w", encoding="utf-8").write("\n".join(s))
    print("graph svg -> %s  (%d crates, %d depends_on edges)" % (OUT_SVG, len(paths), len(edges)))


if __name__ == "__main__":
    main()
