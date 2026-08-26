#!/usr/bin/env python3
"""Render the full Ascendancy card set: one polished SVG (and PNG when a
rasteriser is available) per champion.

HEADLESS. Writes files only -- never opens a window. Run:

    python3 tools/cardgen/render_cards.py            # from aeonrift/
    python3 tools/cardgen/render_cards.py --no-png   # SVG only

Outputs:
    assets/cards/<slug>.svg
    assets/cards/<slug>.png          (if rsvg-convert / cairosvg present)
    assets/cards/index.html          (contact sheet of the whole set)

Illustration hook: if assets/cards/art/<slug>.png exists it is dropped into the
art slot instead of the procedural sigil (see generate_illustrations.py).
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from ron_parse import load_champion, parse_roster_meta  # noqa: E402
from svg_card import build_card_svg  # noqa: E402

# tools/cardgen/ -> tools/ -> aeonrift/
AEON = HERE.parent.parent
CHAMP_DIR = AEON / "content" / "champions"
OUT_DIR = AEON / "assets" / "cards"
ART_DIR = OUT_DIR / "art"


def champion_slugs() -> list[str]:
    """Every top-level ChampionSpec .ron (roster_meta.ron is metadata, not a champion)."""
    slugs = [
        p.stem for p in sorted(CHAMP_DIR.glob("*.ron"))
        if p.stem != "roster_meta"
    ]
    return slugs


def find_rasteriser() -> tuple[str, list] | None:
    """Pick the cheapest available SVG->PNG path; None if we must ship SVG only."""
    if shutil.which("rsvg-convert"):
        return ("rsvg-convert", ["rsvg-convert", "-w", "600", "-h", "840"])
    if shutil.which("cairosvg"):
        return ("cairosvg", ["cairosvg", "-o"])
    try:
        import cairosvg  # noqa: F401
        return ("cairosvg-module", [])
    except Exception:
        return None


def rasterise(kind: str, cmd: list, svg_path: Path, png_path: Path) -> bool:
    try:
        if kind == "rsvg-convert":
            subprocess.run(cmd + ["-o", str(png_path), str(svg_path)],
                           check=True, capture_output=True)
        elif kind == "cairosvg":
            subprocess.run(["cairosvg", str(svg_path), "-o", str(png_path)],
                           check=True, capture_output=True)
        elif kind == "cairosvg-module":
            import cairosvg
            cairosvg.svg2png(url=str(svg_path), write_to=str(png_path),
                             output_width=600, output_height=840)
        return png_path.exists()
    except Exception as e:  # never let one bad rasterise kill the batch
        print(f"    ! raster failed for {svg_path.name}: {e}", file=sys.stderr)
        return False


def build_index(cards: list[dict]) -> str:
    """A simple contact-sheet HTML so the whole set can be eyeballed at once."""
    tiles = []
    for c in sorted(cards, key=lambda c: (c["faction"], c["rarity"], c["name"])):
        tiles.append(
            f'<figure><img src="{c["slug"]}.svg" alt="{c["name"]}" loading="lazy"/>'
            f'<figcaption>{c["name"]} · {c["faction"]} · {c["rarity"]}</figcaption></figure>'
        )
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<title>Aeonrift · Ascendancy — Card Set ({len(cards)})</title>
<style>
  body{{margin:0;background:#0b0910;color:#e6e0f0;font-family:system-ui,sans-serif}}
  header{{padding:24px 32px;border-bottom:1px solid #2a2240}}
  h1{{margin:0;font-size:22px;letter-spacing:2px}}
  p{{margin:6px 0 0;color:#9a90b8}}
  .grid{{display:grid;grid-template-columns:repeat(auto-fill,minmax(220px,1fr));
    gap:22px;padding:28px 32px}}
  figure{{margin:0}}
  img{{width:100%;border-radius:14px;display:block;
    box-shadow:0 8px 24px rgba(0,0,0,.5)}}
  figcaption{{margin-top:8px;font-size:12px;color:#b8aede;text-align:center}}
</style></head><body>
<header><h1>AEONRIFT · ASCENDANCY</h1>
<p>{len(cards)} champion cards · procedural faction/rarity visual system</p></header>
<div class="grid">{''.join(tiles)}</div>
</body></html>"""


def main() -> int:
    ap = argparse.ArgumentParser(description="Render Ascendancy champion cards.")
    ap.add_argument("--no-png", action="store_true", help="emit SVG only")
    ap.add_argument("--only", help="render a single slug (debug)")
    args = ap.parse_args()

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    ART_DIR.mkdir(parents=True, exist_ok=True)

    roster = parse_roster_meta((CHAMP_DIR / "roster_meta.ron").read_text(encoding="utf-8"))
    slugs = champion_slugs()
    if args.only:
        slugs = [s for s in slugs if s == args.only]

    ras = None if args.no_png else find_rasteriser()
    if args.no_png:
        print("PNG rasterisation disabled (--no-png); shipping SVG only.")
    elif ras is None:
        print("No SVG->PNG rasteriser found (rsvg-convert / cairosvg). "
              "Shipping SVG only; see README for install notes.")
    else:
        print(f"Rasterising PNGs via: {ras[0]}")

    cards: list[dict] = []
    png_count = 0
    for slug in slugs:
        model = load_champion(slug, CHAMP_DIR, roster)
        art_png = ART_DIR / f"{slug}.png"
        svg = build_card_svg(model, art_png if art_png.exists() else None)
        svg_path = OUT_DIR / f"{slug}.svg"
        svg_path.write_text(svg, encoding="utf-8")
        cards.append(model)

        note = ""
        if ras is not None:
            if rasterise(ras[0], ras[1], svg_path, OUT_DIR / f"{slug}.png"):
                png_count += 1
                note = " +png"
        art_note = " [AI art]" if art_png.exists() else ""
        print(f"  {model['faction']:>9} {model['rarity']:>9} "
              f"{slug:<26} cost {model['cost']}{note}{art_note}")

    (OUT_DIR / "index.html").write_text(build_index(cards), encoding="utf-8")

    print(f"\nDone: {len(cards)} SVG cards -> {OUT_DIR.relative_to(AEON)}")
    if not args.no_png:
        print(f"      {png_count} PNGs rasterised")
    print(f"      contact sheet -> {(OUT_DIR / 'index.html').relative_to(AEON)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
