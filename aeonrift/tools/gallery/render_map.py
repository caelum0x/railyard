"""Render the MOBA map + a LIVE match snapshot as an SVG (then PNG).

Reads:
  - content/maps/aeonrift_standard.ron   (lanes, turrets, inhibitors, bases, jungle, objectives)
  - screenshots/match_snapshot.json      (live unit positions after ~500 headless ticks)

Draws a top-down diagram: the three lanes, structures, jungle camps and epic
objectives, then overlays the actual live unit positions (champions / minions /
turrets ...) team-colored. Pure stdlib; rasterized separately by rsvg-convert.
"""

import json
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
MAP = os.path.join(ROOT, "content", "maps", "aeonrift_standard.ron")
SNAP = os.path.join(ROOT, "screenshots", "match_snapshot.json")
OUT_SVG = os.path.join(ROOT, "screenshots", "misc", "moba-map-snapshot.svg")

SIZE = 1000
PAD = 60

BLUE = "#3da5ff"   # team 0
RED = "#ff5555"    # team 1
NEUTRAL = "#c9a227"


def read(path):
    return open(path, encoding="utf-8").read()


def points(block):
    return [(float(x), float(y)) for x, y in re.findall(r"P\(x:\s*([\-0-9.]+),\s*y:\s*([\-0-9.]+)\)", block)]


def parse_map(txt):
    bounds = float(re.search(r"bounds:\s*([\-0-9.]+)", txt).group(1))
    m = {"bounds": bounds, "lanes": [], "turrets": [], "inhibitors": [],
         "bases": [], "jungle": [], "objectives": []}
    # bases
    for b in re.finditer(r"BaseDef\(team:\s*(\d+),\s*nexus:\s*P\(x:\s*([\-0-9.]+),\s*y:\s*([\-0-9.]+)\)", txt):
        m["bases"].append((int(b.group(1)), float(b.group(2)), float(b.group(3))))
    # lanes: capture each LaneDef block's waypoints
    for lane in re.finditer(r"LaneDef\((.*?)\n\s*\),?\n\s*(?:LaneDef|\]\,\s*\n\s*jungle)", txt, re.S):
        pass  # fallback below
    # simpler: split on "name:" inside lanes[] — grab each lane's waypoints list
    for lm in re.finditer(r'name:\s*"(top|mid|bot)"\s*,\s*waypoints:\s*\[(.*?)\]', txt, re.S):
        m["lanes"].append((lm.group(1), points(lm.group(2))))
    # turrets: TurretDef(team: t, pos: P(...), tier: n)
    for t in re.finditer(r"TurretDef\(team:\s*(\d+),\s*pos:\s*P\(x:\s*([\-0-9.]+),\s*y:\s*([\-0-9.]+)\),\s*tier:\s*(\d+)", txt):
        m["turrets"].append((int(t.group(1)), float(t.group(2)), float(t.group(3)), int(t.group(4))))
    for t in re.finditer(r"InhibDef\(team:\s*(\d+),\s*pos:\s*P\(x:\s*([\-0-9.]+),\s*y:\s*([\-0-9.]+)\)", txt):
        m["inhibitors"].append((int(t.group(1)), float(t.group(2)), float(t.group(3))))
    for c in re.finditer(r'CampDef\(id:\s*"([^"]+)",\s*pos:\s*P\(x:\s*([\-0-9.]+),\s*y:\s*([\-0-9.]+)\)', txt):
        m["jungle"].append((c.group(1), float(c.group(2)), float(c.group(3))))
    for o in re.finditer(r'ObjectiveDef\(id:\s*"([^"]+)",\s*pos:\s*P\(x:\s*([\-0-9.]+),\s*y:\s*([\-0-9.]+)\)', txt):
        m["objectives"].append((o.group(1), float(o.group(2)), float(o.group(3))))
    return m


def make_tx(bounds):
    span = bounds * 2.0
    scale = (SIZE - 2 * PAD) / span

    def tx(x, y):
        sx = PAD + (x + bounds) * scale
        sy = PAD + (bounds - y) * scale  # flip y so +y is up
        return sx, sy
    return tx


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def team_color(t):
    return BLUE if t == 0 else RED


def main():
    m = parse_map(read(MAP))
    tx = make_tx(m["bounds"])
    snap = json.load(open(SNAP, encoding="utf-8")) if os.path.exists(SNAP) else {"units": [], "tick": 0}

    s = []
    s.append(f'<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}" viewBox="0 0 {SIZE} {SIZE}" font-family="Inter, Segoe UI, sans-serif">')
    s.append(f'<rect width="{SIZE}" height="{SIZE}" fill="#0e1116"/>')
    # arena border
    x0, y0 = tx(-m["bounds"], m["bounds"])
    x1, y1 = tx(m["bounds"], -m["bounds"])
    s.append(f'<rect x="{x0:.1f}" y="{y0:.1f}" width="{x1-x0:.1f}" height="{y1-y0:.1f}" fill="#141922" stroke="#2a3340" stroke-width="2" rx="14"/>')

    # lanes as thick translucent paths
    for name, wps in m["lanes"]:
        if not wps:
            continue
        d = "M " + " L ".join(f"{tx(x,y)[0]:.1f} {tx(x,y)[1]:.1f}" for x, y in wps)
        s.append(f'<path d="{d}" fill="none" stroke="#3a4658" stroke-width="26" stroke-linecap="round" stroke-linejoin="round" opacity="0.55"/>')
        mx, my = tx(*wps[len(wps)//2])
        s.append(f'<text x="{mx:.1f}" y="{my-18:.1f}" fill="#7f8ea3" font-size="18" text-anchor="middle" font-weight="600">{name.upper()}</text>')

    # jungle camps + objectives
    for cid, x, y in m["jungle"]:
        cx, cy = tx(x, y)
        s.append(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="9" fill="#2f7d32" stroke="#8bc34a" stroke-width="2"/>')
    for oid, x, y in m["objectives"]:
        cx, cy = tx(x, y)
        s.append(f'<polygon points="{cx:.1f},{cy-13:.1f} {cx+12:.1f},{cy+8:.1f} {cx-12:.1f},{cy+8:.1f}" fill="#7b2fbf" stroke="#c79bf2" stroke-width="2"/>')
        s.append(f'<text x="{cx:.1f}" y="{cy+26:.1f}" fill="#c79bf2" font-size="13" text-anchor="middle">{esc(oid)}</text>')

    # structures
    for t, x, y, tier in m["turrets"]:
        cx, cy = tx(x, y)
        r = 7 + tier * 2
        s.append(f'<rect x="{cx-r:.1f}" y="{cy-r:.1f}" width="{2*r:.1f}" height="{2*r:.1f}" fill="{team_color(t)}" opacity="0.35" stroke="{team_color(t)}" stroke-width="2"/>')
    for t, x, y in m["inhibitors"]:
        cx, cy = tx(x, y)
        s.append(f'<rect x="{cx-8:.1f}" y="{cy-8:.1f}" width="16" height="16" transform="rotate(45 {cx:.1f} {cy:.1f})" fill="{team_color(t)}" opacity="0.55" stroke="#fff" stroke-width="1.5"/>')
    for t, x, y in m["bases"]:
        cx, cy = tx(x, y)
        s.append(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="18" fill="{team_color(t)}" opacity="0.85" stroke="#fff" stroke-width="2.5"/>')
        s.append(f'<text x="{cx:.1f}" y="{cy+5:.1f}" fill="#0e1116" font-size="14" text-anchor="middle" font-weight="800">N</text>')

    # LIVE units from the snapshot
    live = {"champion": 0, "minion": 0, "turret": 0, "other": 0}
    for u in snap.get("units", []):
        cx, cy = tx(u["x"], u["y"])
        kind = u["kind"]
        col = team_color(u["team"])
        if kind == "champion":
            live["champion"] += 1
            s.append(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="10" fill="{col}" stroke="#fff" stroke-width="2.5"/>')
            s.append(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="15" fill="none" stroke="{col}" stroke-width="1.5" opacity="0.6"/>')
        elif kind.startswith("minion"):
            live["minion"] += 1
            s.append(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="3.5" fill="{col}" opacity="0.9"/>')
        elif kind in ("turret", "inhibitor", "nexus"):
            live["turret"] += 1  # already drawn as structure; skip duplicate marker
        else:
            live["other"] += 1

    # legend / title
    s.append(f'<text x="{PAD}" y="34" fill="#e8edf4" font-size="26" font-weight="800">Aeonrift Arena — live match snapshot</text>')
    s.append(f'<text x="{PAD}" y="{SIZE-24}" fill="#7f8ea3" font-size="15">tick {snap.get("tick",0)} · {live["champion"]} champions alive · {live["minion"]} minions marching · headless deterministic sim (seed 42)</text>')
    # legend swatches
    lx = SIZE - 250
    s.append(f'<circle cx="{lx}" cy="30" r="8" fill="{BLUE}"/><text x="{lx+16}" y="35" fill="#cdd6e2" font-size="15">Team 0</text>')
    s.append(f'<circle cx="{lx+110}" cy="30" r="8" fill="{RED}"/><text x="{lx+126}" y="35" fill="#cdd6e2" font-size="15">Team 1</text>')

    s.append('</svg>')
    os.makedirs(os.path.dirname(OUT_SVG), exist_ok=True)
    open(OUT_SVG, "w", encoding="utf-8").write("\n".join(s))
    print("map svg -> %s  (lanes=%d turrets=%d inhib=%d jungle=%d obj=%d | live champ=%d minion=%d)" % (
        OUT_SVG, len(m["lanes"]), len(m["turrets"]), len(m["inhibitors"]),
        len(m["jungle"]), len(m["objectives"]), live["champion"], live["minion"]))


if __name__ == "__main__":
    main()
