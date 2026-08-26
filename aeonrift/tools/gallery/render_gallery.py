"""Assemble screenshots/GALLERY.html — one scrollable visual overview.

Embeds (via relative <img> src, images live alongside under screenshots/):
  - the project stats infographic
  - the 47 greybox champion model renders, grouped by faction, with name /
    faction / archetype captions
  - the live MOBA map + match snapshot
  - the crate architecture graph

Styling is inlined (no external CSS/JS/CDN) so the file is self-contained.
"""

import html
import json
import os

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
MANIFEST = os.path.join(ROOT, "screenshots", "champions_manifest.json")
OUT = os.path.join(ROOT, "screenshots", "GALLERY.html")

FACTION_COLOR = {
    "Greek": "#d4af37", "Norse": "#5b8fb0", "Roman": "#c1272d", "Egyptian": "#d9a441",
    "Ember": "#ff6b35", "Tide": "#22d3ee", "Verdant": "#43a047", "Umbral": "#7c4dff",
}
FACTION_ORDER = ["Greek", "Norse", "Roman", "Egyptian", "Ember", "Tide", "Verdant", "Umbral"]


def esc(s):
    return html.escape(str(s))


def exists(rel):
    return os.path.exists(os.path.join(ROOT, "screenshots", rel))


def main():
    champs = json.load(open(MANIFEST, encoding="utf-8"))
    by_faction = {}
    for c in champs:
        by_faction.setdefault(c["faction"], []).append(c)

    p = []
    p.append('<!doctype html><html lang="en"><head><meta charset="utf-8">')
    p.append('<meta name="viewport" content="width=device-width, initial-scale=1">')
    p.append('<title>Aeonrift — Visual Gallery</title>')
    p.append('''<style>
:root{color-scheme:dark;}
*{box-sizing:border-box;}
body{margin:0;background:#0b0e13;color:#e8edf4;font-family:Inter,"Segoe UI",system-ui,sans-serif;line-height:1.5;}
.wrap{max-width:1440px;margin:0 auto;padding:40px 28px 90px;}
header h1{font-size:40px;margin:0 0 6px;font-weight:800;letter-spacing:-0.5px;}
header p{color:#8b98ad;margin:0 0 4px;font-size:17px;max-width:900px;}
.note{display:inline-block;margin-top:14px;padding:8px 14px;border:1px solid #b9822d;background:#241a0d;color:#f0c987;border-radius:10px;font-size:14px;}
section{margin-top:56px;}
h2{font-size:26px;font-weight:800;margin:0 0 6px;}
h2 .count{color:#7f8ea3;font-weight:600;font-size:18px;}
.sub{color:#8b98ad;font-size:15px;margin:0 0 20px;}
.full{width:100%;border-radius:16px;border:1px solid #1e2733;background:#12161d;display:block;}
.fac-h{display:flex;align-items:center;gap:12px;margin:34px 0 16px;}
.dot{width:14px;height:14px;border-radius:50%;}
.fac-h h3{font-size:20px;margin:0;font-weight:700;}
.fac-h .n{color:#7f8ea3;font-size:15px;}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(210px,1fr));gap:18px;}
.card{background:#12161d;border:1px solid #1e2733;border-radius:14px;overflow:hidden;transition:transform .12s ease,border-color .12s ease;}
.card:hover{transform:translateY(-3px);border-color:#33465c;}
.card img{width:100%;aspect-ratio:4/5;object-fit:cover;background:#0e1116;display:block;}
.meta{padding:12px 14px 14px;}
.meta .name{font-weight:700;font-size:16px;margin:0 0 8px;}
.badges{display:flex;gap:8px;flex-wrap:wrap;}
.badge{font-size:12px;padding:3px 9px;border-radius:20px;font-weight:600;}
.badge.arch{background:#1b2430;color:#b9c6d8;border:1px solid #2a3746;}
figure{margin:0;}
figcaption{color:#8b98ad;font-size:14px;margin-top:10px;}
footer{margin-top:70px;color:#5c6a7e;font-size:13px;border-top:1px solid #1a222d;padding-top:20px;}
a{color:#5aa9ff;}
</style></head><body><div class="wrap">''')

    # Header
    p.append('<header>')
    p.append('<h1>Aeonrift — Visual Gallery</h1>')
    p.append('<p>Everything built so far, rendered fully headless: the 47-champion roster as clean studio portraits, a live MOBA match snapshot from the deterministic sim, the crate architecture, and the content stats.</p>')
    p.append('<div class="note">Honest caveat: the champion 3D models are <strong>greybox procedural</strong> stand-ins (shaped by archetype, tinted by faction) — final art is pending. Everything here is a rendered image file; no windows were opened.</div>')
    p.append('</header>')

    # Stats
    if exists("misc/stats-infographic.png"):
        p.append('<section><h2>Project at a glance</h2>')
        p.append('<p class="sub">Counts read live from <code>content/*.ron</code> and the workspace Cargo manifests.</p>')
        p.append('<img class="full" src="misc/stats-infographic.png" alt="Aeonrift stats infographic">')
        p.append('</section>')

    # Champion grid
    p.append(f'<section><h2>Champion models <span class="count">· {len(champs)} greybox renders</span></h2>')
    p.append('<p class="sub">EEVEE studio renders (800×1000) of the actual shipped <code>.glb</code> models. Proportions come from the archetype; the tint is the faction color.</p>')
    for fac in FACTION_ORDER + [f for f in by_faction if f not in FACTION_ORDER]:
        group = sorted(by_faction.get(fac, []), key=lambda c: c["name"])
        if not group:
            continue
        col = FACTION_COLOR.get(fac, "#8a8a92")
        p.append(f'<div class="fac-h"><span class="dot" style="background:{col}"></span><h3>{esc(fac)}</h3><span class="n">{len(group)} champions</span></div>')
        p.append('<div class="grid">')
        for c in group:
            rel = "champions/%s.png" % c["slug"]
            p.append('<div class="card">')
            p.append(f'<img loading="lazy" src="{esc(rel)}" alt="{esc(c["name"])}">')
            p.append('<div class="meta">')
            p.append(f'<p class="name">{esc(c["name"])}</p>')
            p.append('<div class="badges">')
            p.append(f'<span class="badge" style="background:{col}22;color:{col};border:1px solid {col}66">{esc(c["faction"])}</span>')
            p.append(f'<span class="badge arch">{esc(c["archetype"])}</span>')
            p.append('</div></div></div>')
        p.append('</div>')
    p.append('</section>')

    # Map snapshot
    if exists("misc/moba-map-snapshot.png"):
        p.append('<section><h2>Live MOBA match snapshot</h2>')
        p.append('<p class="sub">A Normal 5v5 <code>Match</code> stepped ~500 deterministic ticks headless (seed 42), then the live unit positions plotted over the parsed map geometry.</p>')
        p.append('<figure><img class="full" style="max-width:820px;margin:0 auto" src="misc/moba-map-snapshot.png" alt="MOBA map + live match snapshot">')
        p.append('<figcaption>Three lanes, tier-1/2 turrets (squares), inhibitors (diamonds) and nexuses (circles) team-colored; jungle camps (green) and epic objectives (triangles); small dots are live minions marching, ringed dots are surviving champions.</figcaption></figure>')
        p.append('</section>')

    # Architecture
    if exists("misc/architecture-graph.png"):
        p.append('<section><h2>Crate architecture</h2>')
        p.append('<p class="sub">The workspace crates clustered by directory, with the real <code>DEPENDS_ON</code> edges from the repo knowledge-graph tool (<code>aeonrift-graph</code>).</p>')
        p.append('<figure><img class="full" src="misc/architecture-graph.png" alt="Crate architecture graph">')
        p.append('<figcaption>Graphviz <code>dot</code> was not installed, so this is a hand-laid architecture view. The complete knowledge graph (crates + champions + abilities + assets, 800+ edges) is exported to <code>misc/architecture-graph.dot</code> and <code>misc/architecture-graph.mermaid</code>.</figcaption></figure>')
        p.append('</section>')

    p.append('<footer>Generated headlessly on the <code>aeon/visual-gallery</code> branch. Cards were skipped: no <code>assets/cards/*.svg</code> exist on <code>aeon/card-visuals</code> yet. Champion art is greybox — final models pending.</footer>')
    p.append('</div></body></html>')

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    open(OUT, "w", encoding="utf-8").write("\n".join(p))
    print("gallery -> %s  (%d champions across %d factions)" % (OUT, len(champs), len(by_faction)))


if __name__ == "__main__":
    main()
