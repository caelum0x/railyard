"""Generate assets/champions/MODELS.html — a faction-grouped gallery of the AI 3D
results (preview.png render + reference.png concept), one file to inspect everything.
Purely local file references; no window is ever opened.
"""
from __future__ import annotations

import html
from pathlib import Path

import common

FACTION_ORDER = [
    "Greek", "Norse", "Roman", "Egyptian",
    "Ember", "Tide", "Verdant", "Umbral",
]
FACTION_COLOR = {
    "Greek": "#d4af37", "Norse": "#6f9fd0", "Roman": "#c0392b",
    "Egyptian": "#e0b872", "Ember": "#e67e22", "Tide": "#1ab4c4",
    "Verdant": "#3fa34d", "Umbral": "#9b59b6",
}


def card(slug: str, name: str, has_model: bool) -> str:
    cdir = common.CHAMPS_ASSETS / slug
    preview = "preview.png" if (cdir / "preview.png").exists() else None
    reference = "reference.png" if (cdir / "reference.png").exists() else None
    badge = "AI 3D" if has_model else "greybox"
    badge_cls = "ok" if has_model else "miss"
    prev_html = (
        f'<img loading="lazy" src="{slug}/{preview}" alt="{html.escape(name)} 3D render">'
        if preview else '<div class="noimg">no preview</div>'
    )
    ref_html = (
        f'<img loading="lazy" src="{slug}/{reference}" alt="{html.escape(name)} concept">'
        if reference else '<div class="noimg">no reference</div>'
    )
    return f'''
      <div class="card">
        <div class="imgs">
          <figure><figcaption>3D render</figcaption>{prev_html}</figure>
          <figure><figcaption>concept ref</figcaption>{ref_html}</figure>
        </div>
        <div class="meta">
          <span class="name">{html.escape(name)}</span>
          <span class="badge {badge_cls}">{badge}</span>
          <span class="slug">{html.escape(slug)}</span>
        </div>
      </div>'''


def main() -> None:
    meta_all = common.roster_meta()
    by_faction: dict[str, list[str]] = {}
    for slug in common.slugs():
        fac = meta_all.get(slug, {}).get("faction", "Other")
        by_faction.setdefault(fac, []).append(slug)

    total = len(common.slugs())
    ai_count = sum(
        1 for slug in common.slugs()
        if (common.CHAMPS_ASSETS / slug / ".ai3d.done").exists()
    )

    sections = []
    ordered = [f for f in FACTION_ORDER if f in by_faction] + [
        f for f in by_faction if f not in FACTION_ORDER
    ]
    for fac in ordered:
        color = FACTION_COLOR.get(fac, "#888")
        cards = "".join(
            card(slug, common.champion_name(slug),
                 (common.CHAMPS_ASSETS / slug / ".ai3d.done").exists())
            for slug in sorted(by_faction[fac])
        )
        sections.append(f'''
    <section>
      <h2 style="border-color:{color}"><span style="color:{color}">&#9670;</span> {html.escape(fac)}
        <small>({len(by_faction[fac])} champions)</small></h2>
      <div class="grid">{cards}</div>
    </section>''')

    doc = f'''<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Aeonrift — AI 3D Champion Models</title>
<style>
  :root {{ color-scheme: dark; }}
  body {{ margin:0; background:#0e0e12; color:#e8e8ee;
    font:15px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif; }}
  header {{ padding:28px 32px; border-bottom:1px solid #23232b; }}
  header h1 {{ margin:0 0 6px; font-size:24px; }}
  header p {{ margin:0; color:#9a9aa8; }}
  .stat {{ color:#3fa34d; font-weight:600; }}
  main {{ padding:12px 32px 64px; }}
  section {{ margin:36px 0; }}
  h2 {{ font-size:19px; border-left:4px solid #888; padding-left:12px; margin:0 0 16px; }}
  h2 small {{ color:#7a7a88; font-weight:400; }}
  .grid {{ display:grid; gap:18px;
    grid-template-columns:repeat(auto-fill,minmax(300px,1fr)); }}
  .card {{ background:#16161c; border:1px solid #26262f; border-radius:12px;
    overflow:hidden; }}
  .imgs {{ display:grid; grid-template-columns:1fr 1fr; gap:1px; background:#26262f; }}
  figure {{ margin:0; background:#0b0b0f; position:relative; }}
  figcaption {{ position:absolute; top:6px; left:6px; font-size:11px; color:#c8c8d4;
    background:rgba(0,0,0,.55); padding:2px 7px; border-radius:6px; }}
  .imgs img {{ display:block; width:100%; aspect-ratio:3/4; object-fit:cover; }}
  .noimg {{ display:flex; align-items:center; justify-content:center; aspect-ratio:3/4;
    color:#555; font-size:12px; }}
  .meta {{ display:flex; align-items:center; gap:8px; padding:10px 12px; }}
  .name {{ font-weight:600; }}
  .slug {{ color:#6a6a78; font-size:12px; margin-left:auto; }}
  .badge {{ font-size:11px; padding:2px 8px; border-radius:20px; }}
  .badge.ok {{ background:#123d1c; color:#5cd67a; }}
  .badge.miss {{ background:#3d1212; color:#e07a7a; }}
</style></head>
<body>
<header>
  <h1>Aeonrift — AI-Generated 3D Champion Models</h1>
  <p><span class="stat">{ai_count}/{total}</span> champions with real image-to-3D models
     (Flux concept &rarr; TRELLIS textured GLB). Left = headless Blender render of the
     new <code>model.glb</code>; right = the Flux concept reference.</p>
  <p style="margin-top:6px;color:#7a7a88">These are STATIC / un-rigged meshes — good for
     display &amp; prototype; need retopo + rigging for in-game animation.</p>
</header>
<main>{"".join(sections)}</main>
</body></html>'''

    out = common.CHAMPS_ASSETS / "MODELS.html"
    out.write_text(doc)
    print(f"wrote {out} ({ai_count}/{total} ai-3d)")


if __name__ == "__main__":
    main()
