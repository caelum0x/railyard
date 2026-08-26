"""Rebuild MODELS-HQ.html for the matte-v2 pass (worktree rail-matte).

Regenerates the faction-grouped gallery from champion_meta + the ORIGINAL 47-champion
finalize summary, overlaid with the matte-v2 per-target decisions so the affected entries
(new v2 Hunyuan render, or a flip to TRELLIS fallback) are rebuilt while the other 36
entries render identically. Output layout/markup matches the committed generator verbatim.
"""
from __future__ import annotations

import html
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE / "out"
HQ_OUT = Path("/Users/arhansubasi/products/rail-matte/aeonrift/tools/aeonrift_3d_hq/out")
ASSETS = Path("/Users/arhansubasi/products/rail-matte/aeonrift/assets/champions")
GALLERY = ASSETS / "MODELS-HQ.html"

FACTION_ORDER = ["Greek", "Norse", "Roman", "Egyptian", "Ember", "Tide", "Verdant", "Umbral"]
FACTION_COLOR = {
    "Greek": "#d4af37", "Norse": "#6f9fd0", "Roman": "#c0392b", "Egyptian": "#e0b872",
    "Ember": "#e67e22", "Tide": "#1ab4c4", "Verdant": "#3fa34d", "Umbral": "#9b59b6",
}

HEAD = """<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Aeonrift — Hunyuan3D-2.1 HQ Champion Models</title>
<style>
  :root {{ color-scheme: dark; }}
  body {{ margin:0; background:#0e0e12; color:#e8e8ee;
    font:15px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif; }}
  header {{ padding:28px 32px; border-bottom:1px solid #23232b; }}
  header h1 {{ margin:0 0 6px; font-size:24px; }}
  header p {{ margin:0; color:#9a9aa8; }}
  .stat {{ color:#5cd67a; font-weight:600; }}
  .statb {{ color:#e0b872; font-weight:600; }}
  main {{ padding:12px 32px 64px; }}
  section {{ margin:36px 0; }}
  h2 {{ font-size:19px; border-left:4px solid #888; padding-left:12px; margin:0 0 16px; }}
  h2 small {{ color:#7a7a88; font-weight:400; }}
  .grid {{ display:grid; gap:18px;
    grid-template-columns:repeat(auto-fill,minmax(300px,1fr)); }}
  .card {{ background:#16161c; border:1px solid #26262f; border-radius:12px;
    overflow:hidden; }}
  .card.fallback {{ border-color:#5a4620; }}
  .imgs {{ display:grid; grid-template-columns:1fr 1fr; gap:1px; background:#26262f; }}
  figure {{ margin:0; background:#0b0b0f; position:relative; }}
  figcaption {{ position:absolute; top:6px; left:6px; font-size:11px; color:#c8c8d4;
    background:rgba(0,0,0,.55); padding:2px 7px; border-radius:6px; }}
  .imgs img {{ display:block; width:100%; aspect-ratio:3/4; object-fit:cover; }}
  .meta {{ display:flex; align-items:center; gap:8px; padding:10px 12px; }}
  .name {{ font-weight:600; }}
  .slug {{ color:#6a6a78; font-size:12px; margin-left:auto; }}
  .badge {{ font-size:10px; letter-spacing:.4px; padding:3px 9px; border-radius:20px;
    font-weight:600; }}
  .badge.hunyuan {{ background:#123d1c; color:#5cd67a; }}
  .badge.fallback {{ background:#3d2f12; color:#e0b872; }}
</style></head>
<body>
<header>
  <h1>Aeonrift — Hunyuan3D-2.1 HQ Champion Models</h1>
  <p><span class="stat">{n_hunyuan}</span> shipped on <code>ndreca/hunyuan3d-2.1</code>
     &middot; <span class="statb">{n_fallback}</span> kept on TRELLIS fallback where Hunyuan
     mangled the champion. Left = Flux concept reference; right = headless Blender render of
     the new Hunyuan <code>model.glb</code>.</p>
  <p style="margin-top:6px;color:#7a7a88">Higher-fidelity re-gen (PBR textures, ~40k tris,
     complex costumes/capes) over the previous TRELLIS pass. Static / un-rigged meshes.
     Matte-v2 pass: tighter background cut-outs to remove residual white base geometry.</p>
</header>
<main>
"""

CARD = """      <div class="card{fallback_cls}">
        <div class="imgs">
          <figure><figcaption>concept ref</figcaption><img loading="lazy" src="{slug}/reference.png" alt="{name} concept"></figure>
          <figure><figcaption>{mid_caption}</figcaption><img loading="lazy" src="{slug}/{mid_img}" alt="{name} render"></figure>
        </div>
        <div class="meta">
          <span class="name">{name}</span>
          <span class="badge {badge_cls}">{badge_text}</span>
          <span class="slug">{slug}</span>
        </div>
      </div>
"""


def main() -> int:
    meta_all = json.loads((HQ_OUT / "champion_meta.json").read_text())
    base = json.loads((HQ_OUT / "finalize_summary.json").read_text())
    v2 = json.loads((OUT / "finalize_v2_summary.json").read_text())

    fallback = set(base.get("trellis_fallback", []))
    # Apply matte-v2 decisions: v2 trellis picks -> fallback; hunyuan_v2/prev -> not fallback.
    for slug in v2.get("trellis", []):
        fallback.add(slug)
    for slug in v2.get("hunyuan_v2", []) + v2.get("hunyuan_prev", []):
        fallback.discard(slug)

    by_faction: dict[str, list[str]] = {}
    for slug, m in meta_all.items():
        by_faction.setdefault(m["faction"], []).append(slug)

    n_fallback = len(fallback)
    n_hunyuan = len(meta_all) - n_fallback

    parts = [HEAD.format(n_hunyuan=n_hunyuan, n_fallback=n_fallback)]
    for faction in FACTION_ORDER:
        slugs = sorted(by_faction.get(faction, []), key=lambda s: meta_all[s]["champion"])
        if not slugs:
            continue
        color = FACTION_COLOR.get(faction, "#888")
        parts.append(
            f'    <section>\n      <h2 style="border-color:{color}">'
            f'<span style="color:{color}">&#9670;</span> {faction} '
            f'<small>({len(slugs)} champions)</small></h2>\n      <div class="grid">\n')
        for slug in slugs:
            name = html.escape(meta_all[slug]["champion"] or slug)
            is_fb = slug in fallback
            has_hq = (ASSETS / slug / "preview.hunyuan.png").is_file()
            if has_hq:
                mid_img, mid_caption = "preview.hunyuan.png", "Hunyuan3D render"
            else:
                mid_img, mid_caption = "preview.png", "TRELLIS (no HQ regen)"
            parts.append(CARD.format(
                slug=slug, name=name,
                fallback_cls=" fallback" if is_fb else "",
                badge_cls="fallback" if is_fb else "hunyuan",
                badge_text="TRELLIS-FALLBACK" if is_fb else "HUNYUAN",
                mid_img=mid_img, mid_caption=mid_caption))
        parts.append("      </div>\n    </section>\n")
    parts.append("</main>\n</body></html>\n")

    GALLERY.write_text("".join(parts))
    print(f"wrote {GALLERY} ({n_hunyuan} hunyuan, {n_fallback} fallback)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
