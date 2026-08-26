#!/usr/bin/env python3
"""Composite the 47 painterly champion splashes into FINISHED Ascendancy trading
cards -- a premium, mobile-card (Hearthstone / Marvel-Snap-grade) layout.

HEADLESS. No windows, no screencapture. Writes files only.

Pipeline per champion:
  1. Load the card model (name / faction / rarity / cost / stats / abilities)
     from the shipped RON content via the existing ron_parse loader.
  2. Smart-crop its painterly splash PNG into the card's tall art aperture,
     biased to keep the character's head + torso (Pillow, cover-fit).
  3. Assemble a self-contained SVG card: faction-coloured ornate border, rarity
     gem + glow + filigree, cost pip, name/title, four stat icons, and an
     ability text box -- pulling ALL colour/typography from the shared faction &
     rarity design tokens so the 47 cards read as ONE intentional set.
  4. Rasterise to a high-res PNG with rsvg-convert.

Reuses the existing visual system (factions.py tokens, ron_parse.py loader);
only the art aperture is re-proportioned portrait so full character splashes
show properly instead of a cropped landscape band.

    python3 tools/cardgen/compose_final.py
    python3 tools/cardgen/compose_final.py --only zeus
"""

from __future__ import annotations

import argparse
import base64
import html
import io
import math
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from factions import faction_tokens, rarity_tokens  # noqa: E402
from ron_parse import load_champion, parse_roster_meta  # noqa: E402

# tools/cardgen/ -> tools/ -> aeonrift/
AEON = HERE.parent.parent
CHAMP_DIR = AEON / "content" / "champions"
OUT_DIR = AEON / "assets" / "cards" / "final-v2"
ART_STAGE = OUT_DIR / "art-crops"  # intermediate cropped splashes (gitignored)

# Painterly splash inputs live in the sibling splash-style workspace.
SPLASH_DIR = Path(
    "/Users/arhansubasi/products/rail-splashstyle/aeonrift/assets/cards/style-test"
)

# --- Card geometry (5:7 portrait). Rasterised at RENDER_SCALE. ----------------
W, H = 750, 1050
RENDER_SCALE = 2  # -> 1500 x 2100 final PNG

# Art aperture (tall, character-forward). Width shrinks slightly for chunky
# legendary frames so the art never collides with the border.
ART_Y, ART_H = 146, 540
CROP_FOCUS_X, CROP_FOCUS_Y = 0.50, 0.42  # keep head+torso, drop sky & legs


def _esc(s: object) -> str:
    return html.escape(str(s), quote=True)


def _diamond(cx: float, cy: float, r: float) -> str:
    return f"{cx},{cy-r} {cx+r},{cy} {cx},{cy+r} {cx-r},{cy}"


def _star_pts(cx: float, cy: float, ro: float, ri: float, n: int) -> str:
    pts = []
    for i in range(n * 2):
        r = ro if i % 2 == 0 else ri
        a = math.radians(-90 + i * 180 / n)
        pts.append(f"{cx+r*math.cos(a):.1f},{cy+r*math.sin(a):.1f}")
    return " ".join(pts)


def _n(v: object) -> str:
    if v is None:
        return "-"
    f = float(v)  # type: ignore[arg-type]
    return str(int(f)) if f == int(f) else f"{f:g}"


# --- Splash -> art aperture (Pillow cover-crop, character-biased) --------------

def cover_crop(im: Image.Image, tw: int, th: int, fx: float, fy: float) -> Image.Image:
    """Scale to COVER (tw x th) then crop, biasing the crop toward (fx, fy).

    fy < 0.5 keeps more of the top of the source (the character's head)."""
    sw, sh = im.size
    scale = max(tw / sw, th / sh)
    nw, nh = max(tw, round(sw * scale)), max(th, round(sh * scale))
    im = im.resize((nw, nh), Image.LANCZOS)
    max_x, max_y = nw - tw, nh - th
    left = int(round(max_x * fx))
    top = int(round(max_y * fy))
    left = max(0, min(left, max_x))
    top = max(0, min(top, max_y))
    return im.crop((left, top, left + tw, top + th))


def art_data_uri(splash: Path, crop_w: int, crop_h: int, save_to: Path) -> tuple[str, dict]:
    """Produce the cropped art as a base64 PNG data-URI (self-contained SVG).

    Returns (uri, crop_report) where crop_report notes headroom kept, so callers
    can flag splashes that cropped badly."""
    im = Image.open(splash).convert("RGB")
    src_w, src_h = im.size
    # render the crop at 2x aperture resolution for a crisp raster
    tw, th = crop_w * RENDER_SCALE, crop_h * RENDER_SCALE
    cropped = cover_crop(im, tw, th, CROP_FOCUS_X, CROP_FOCUS_Y)

    save_to.parent.mkdir(parents=True, exist_ok=True)
    cropped.save(save_to, format="PNG", optimize=True)

    buf = io.BytesIO()
    cropped.save(buf, format="PNG", optimize=True)
    b64 = base64.b64encode(buf.getvalue()).decode("ascii")

    # crop report: how much of the source's vertical extent survived, and where
    scale = max(tw / src_w, th / src_h)
    kept_frac = min(1.0, (th / scale) / src_h)
    report = {"src": (src_w, src_h), "kept_v_frac": round(kept_frac, 3)}
    return f"data:image/png;base64,{b64}", report


# --- small stat icon glyphs (24x24 box, translated into place) ----------------

def _icon(kind: str, x: float, y: float, color: str, scale: float = 1.0) -> str:
    p = {
        "hp": '<path d="M12 21 C4 14 4 8 8 6 C10 5 12 7 12 8 C12 7 14 5 16 6 C20 8 20 14 12 21 Z"/>',
        "ad": '<path d="M4 20 L14 10 M12 4 L20 4 L20 12 M16 8 L20 4 M4 20 L7 17"/>',
        "armor": '<path d="M12 3 L20 6 V12 C20 17 16 20 12 21 C8 20 4 17 4 12 V6 Z"/>',
        "range": '<path d="M12 2 A10 10 0 1 0 12 22 A10 10 0 1 0 12 2 M12 7 V17 M7 12 H17"/>',
    }[kind]
    fill = "none" if kind in ("ad", "range") else color
    return (f'<g transform="translate({x:.1f},{y:.1f}) scale({scale})" '
            f'fill="{fill}" stroke="{color}" stroke-width="1.6" '
            f'stroke-linejoin="round" stroke-linecap="round">{p}</g>')


_TARGET_TAG = {
    "Area": "AOE", "Skillshot": "SKILL", "SelfCast": "SELF", "Target": "TARGET",
}


def _defs(fac: dict, rar: dict) -> str:
    return f'''<defs>
  <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{fac['bg0']}"/>
    <stop offset="1" stop-color="{fac['bg1']}"/>
  </linearGradient>
  <linearGradient id="frame" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0"   stop-color="{fac['frame0']}"/>
    <stop offset="0.5" stop-color="{fac['frame1']}"/>
    <stop offset="1"   stop-color="{fac['frame0']}"/>
  </linearGradient>
  <linearGradient id="ribbon" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{fac['frame0']}"/>
    <stop offset="1" stop-color="{fac['frame1']}"/>
  </linearGradient>
  <radialGradient id="cost" cx="0.4" cy="0.35" r="0.8">
    <stop offset="0" stop-color="{fac['light']}"/>
    <stop offset="1" stop-color="{fac['frame1']}"/>
  </radialGradient>
  <radialGradient id="gem" cx="0.35" cy="0.3" r="0.8">
    <stop offset="0" stop-color="{rar['gem_light']}"/>
    <stop offset="1" stop-color="{rar['gem']}"/>
  </radialGradient>
  <linearGradient id="scrim" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0"    stop-color="{fac['bg1']}" stop-opacity="0"/>
    <stop offset="0.72" stop-color="{fac['bg1']}" stop-opacity="0"/>
    <stop offset="1"    stop-color="{fac['bg1']}" stop-opacity="0.85"/>
  </linearGradient>
  <linearGradient id="topscrim" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0"   stop-color="{fac['bg1']}" stop-opacity="0.7"/>
    <stop offset="1"   stop-color="{fac['bg1']}" stop-opacity="0"/>
  </linearGradient>
  <filter id="soft" x="-25%" y="-25%" width="150%" height="150%">
    <feGaussianBlur stdDeviation="7"/>
  </filter>
</defs>'''


def _frame(fac: dict, rar: dict, fw: int) -> str:
    glow = ""
    if rar["glow"]:
        glow = (f'<rect x="7" y="7" width="{W-14}" height="{H-14}" rx="36" '
                f'fill="none" stroke="{rar["gem"]}" stroke-width="12" '
                f'opacity="0.55" filter="url(#soft)"/>')
    body = f'<rect x="5" y="5" width="{W-10}" height="{H-10}" rx="34" fill="url(#frame)"/>'
    inner = (f'<rect x="{fw}" y="{fw}" width="{W-2*fw}" height="{H-2*fw}" '
             f'rx="24" fill="url(#bg)"/>')
    band = (f'<rect x="{fw-3}" y="{fw-3}" width="{W-2*fw+6}" height="{H-2*fw+6}" '
            f'rx="26" fill="none" stroke="{rar["band"]}" stroke-width="1.6" opacity="0.9"/>')
    orn = _ornaments(rar) if rar["ornaments"] else ""
    return glow + body + inner + band + orn


def _ornaments(rar: dict) -> str:
    c = rar["gem"]
    corners = [(34, 34, 1, 1), (W - 34, 34, -1, 1),
               (34, H - 34, 1, -1), (W - 34, H - 34, -1, -1)]
    out = []
    for cx, cy, sx, sy in corners:
        out.append(
            f'<path d="M{cx},{cy+26*sy} q0,{-26*sy} {26*sx},{-26*sy} '
            f'M{cx+7*sx},{cy+26*sy} q0,{-19*sy} {19*sx},{-19*sy}" '
            f'fill="none" stroke="{c}" stroke-width="2.8" opacity="0.85"/>'
            f'<circle cx="{cx}" cy="{cy}" r="4" fill="{c}"/>')
    return "".join(out)


def _art(fac: dict, rar: dict, uri: str, ax: int, aw: int) -> str:
    x, y, w, h = ax, ART_Y, aw, ART_H
    cid = "artclip"
    clip = f'<clipPath id="{cid}"><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="16"/></clipPath>'
    img = (f'<g clip-path="url(#{cid})">'
           f'<image x="{x}" y="{y}" width="{w}" height="{h}" '
           f'href="{uri}" preserveAspectRatio="xMidYMid slice"/>'
           # bottom scrim so the nameplate/ribbon transition reads cleanly
           f'<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="url(#scrim)"/>'
           f'<rect x="{x}" y="{y}" width="{w}" height="70" fill="url(#topscrim)"/>'
           f'</g>')
    bevel = (f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="16" fill="none" '
             f'stroke="{fac["frame1"]}" stroke-width="5"/>'
             f'<rect x="{x+4}" y="{y+4}" width="{w-8}" height="{h-8}" rx="13" fill="none" '
             f'stroke="{rar["band"]}" stroke-width="1.3" opacity="0.7"/>')
    rivets = "".join(
        f'<circle cx="{rx}" cy="{ry}" r="5" fill="{fac["frame0"]}" stroke="{fac["bg1"]}" stroke-width="1.2"/>'
        for rx, ry in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)]
    )
    return clip + img + bevel + rivets


def _header(m: dict, fac: dict, rar: dict) -> str:
    name = _esc(m["name"]).upper()
    title = _esc(m["title"])
    # cost badge (top-left)
    cost = (f'<g>'
            f'<circle cx="84" cy="94" r="50" fill="url(#cost)" stroke="{fac["frame1"]}" stroke-width="3.5"/>'
            f'<circle cx="84" cy="94" r="50" fill="none" stroke="{fac["light"]}" stroke-width="1.2" opacity="0.6"/>'
            f'<polygon points="{_diamond(84,60,8)}" fill="{fac["bg1"]}" opacity="0.85"/>'
            f'<text x="84" y="96" font-family="Georgia, serif" font-size="48" font-weight="700" '
            f'fill="{fac["bg1"]}" text-anchor="middle" dominant-baseline="central">{m["cost"]}</text>'
            f'</g>')
    # rarity gem (top-right) + rank pips
    gx, gy = 666, 80
    pips = "".join(
        f'<circle cx="{gx-16+i*11}" cy="{gy+36}" r="4" fill="{rar["gem"]}" '
        f'stroke="{fac["bg1"]}" stroke-width="0.9"/>'
        for i in range(rar["pips"])
    )
    gem = (f'<g>'
           f'<polygon points="{_diamond(gx,gy,27)}" fill="url(#gem)" '
           f'stroke="{fac["frame1"]}" stroke-width="3"/>'
           f'<polygon points="{_diamond(gx,gy,27)}" fill="none" stroke="{rar["gem_light"]}" '
           f'stroke-width="1.2" opacity="0.8"/>'
           f'<polygon points="{gx-27},{gy} {gx},{gy-27} {gx},{gy}" fill="{rar["gem_light"]}" opacity="0.35"/>'
           f'{pips}</g>')
    name_txt = (f'<text x="{W/2+6}" y="70" font-family="Georgia, \'Times New Roman\', serif" '
                f'font-size="40" font-weight="700" letter-spacing="1.5" '
                f'fill="{fac["light"]}" text-anchor="middle" '
                f'style="paint-order:stroke;stroke:{fac["bg1"]};stroke-width:3.5px">{name}</text>')
    title_txt = (f'<text x="{W/2+6}" y="106" font-family="Georgia, serif" font-size="19" '
                 f'font-style="italic" fill="{fac["primary"]}" text-anchor="middle" '
                 f'letter-spacing="0.5">{title}</text>') if title else ""
    divider = (f'<line x1="150" y1="128" x2="600" y2="128" stroke="{fac["frame1"]}" '
               f'stroke-width="1.6" opacity="0.6"/>'
               f'<polygon points="{_diamond(375,128,6)}" fill="{fac["primary"]}"/>')
    return cost + gem + name_txt + title_txt + divider


def _ribbon(m: dict, fac: dict) -> str:
    y = 694
    left = _esc(fac["label"])
    right = _esc(m["archetype"]).upper()
    plate = (f'<path d="M74,{y} L676,{y} L660,{y+40} L90,{y+40} Z" '
             f'fill="url(#ribbon)" stroke="{fac["bg1"]}" stroke-width="1.6"/>')
    ftext = (f'<text x="108" y="{y+27}" font-family="Georgia, serif" font-size="17" '
             f'font-weight="700" letter-spacing="1.3" fill="{fac["bg1"]}">{left}</text>')
    atext = (f'<text x="642" y="{y+27}" font-family="Georgia, serif" font-size="17" '
             f'font-weight="700" letter-spacing="1.3" fill="{fac["bg1"]}" '
             f'text-anchor="end">{right}</text>')
    dot = f'<polygon points="{_diamond(375,y+20,8)}" fill="{fac["bg1"]}"/>'
    return plate + ftext + atext + dot


def _stats(m: dict, fac: dict) -> str:
    s = m["stats"]
    cells = [
        ("hp", "HP", _n(s.get("hp"))),
        ("ad", "AD", _n(s.get("ad"))),
        ("armor", "ARM", _n(s.get("armor"))),
        ("range", "RNG", _n(s.get("range"))),
    ]
    y = 744
    cw, gap = 158, 8
    total = 4 * cw + 3 * gap
    x0 = (W - total) / 2
    out = []
    for i, (icon, lbl, val) in enumerate(cells):
        cx = x0 + i * (cw + gap)
        out.append(
            f'<rect x="{cx}" y="{y}" width="{cw}" height="58" rx="11" '
            f'fill="{fac["bg1"]}" stroke="{fac["frame1"]}" stroke-width="1.6" opacity="0.94"/>'
            + _icon(icon, cx + 12, y + 16, fac["primary"], 1.05)
            + f'<text x="{cx+44}" y="{y+24}" font-family="sans-serif" font-size="12" '
              f'fill="{fac["primary"]}" font-weight="600" letter-spacing="1">{lbl}</text>'
            + f'<text x="{cx+44}" y="{y+46}" font-family="Georgia, serif" font-size="23" '
              f'font-weight="700" fill="{fac["light"]}">{val}</text>'
        )
    return "".join(out)


def _abilities(m: dict, fac: dict) -> str:
    x, y, w, h = 36, 812, W - 72, 188
    panel = (f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="16" '
             f'fill="{fac["bg1"]}" stroke="{fac["frame1"]}" stroke-width="2.8" opacity="0.96"/>')
    header = (f'<text x="{x+22}" y="{y+28}" font-family="Georgia, serif" font-size="15" '
              f'font-weight="700" letter-spacing="2" fill="{fac["primary"]}">ABILITIES</text>'
              f'<line x1="{x+22}" y1="{y+37}" x2="{x+w-22}" y2="{y+37}" '
              f'stroke="{fac["frame1"]}" stroke-width="1" opacity="0.5"/>')
    rows = []
    abilities = m["abilities"][:3]
    body_h = h - 46
    row_h = body_h / max(1, len(abilities))
    for i, ab in enumerate(abilities):
        ry = y + 50 + i * row_h
        aname = _esc(ab["name"])
        eff = " · ".join(ab["effects"]) if ab["effects"] else "Utility"
        tag = _TARGET_TAG.get(ab.get("targeting") or "", "")
        cost = ab.get("cost")
        chip = ""
        if tag:
            chip = (f'<rect x="{x+w-96}" y="{ry+2}" width="76" height="22" rx="11" '
                    f'fill="none" stroke="{fac["frame1"]}" stroke-width="1.3"/>'
                    f'<text x="{x+w-58}" y="{ry+17}" font-family="sans-serif" font-size="11" '
                    f'font-weight="700" letter-spacing="1" fill="{fac["primary"]}" '
                    f'text-anchor="middle">{tag}</text>')
        cost_txt = f'  ·  {int(cost)} mana' if cost else ""
        rows.append(
            f'<text x="{x+22}" y="{ry+16}" font-family="Georgia, serif" font-size="18" '
            f'font-weight="700" fill="{fac["light"]}">{aname}</text>'
            f'{chip}'
            f'<text x="{x+22}" y="{ry+38}" font-family="sans-serif" font-size="13.5" '
            f'fill="{fac["primary"]}" opacity="0.92">{_esc(eff)}{cost_txt}</text>'
        )
    return panel + header + "".join(rows)


def _footer(m: dict, fac: dict, rar: dict, fw: int) -> str:
    y = 1026  # fixed baseline: clears the ability panel, inside even the legendary frame
    left = (f'<text x="40" y="{y}" font-family="Georgia, serif" font-size="13" '
            f'letter-spacing="2" fill="{fac["primary"]}" opacity="0.85">'
            f'AEONRIFT &#183; ASCENDANCY</text>')
    star = f'<polygon points="{_star_pts(675, y-5, 9, 3.8, 5)}" fill="{rar["gem"]}"/>'
    right = (f'<text x="655" y="{y}" font-family="Georgia, serif" font-size="13" '
             f'font-weight="700" letter-spacing="1.5" fill="{rar["gem"]}" '
             f'text-anchor="end">{_esc(rar["label"])}</text>')
    return left + right + star


def build_card_svg(model: dict, art_uri: str, fw: int, ax: int, aw: int) -> str:
    fac = faction_tokens(model["faction"])
    rar = rarity_tokens(model["rarity"])
    body = (
        _defs(fac, rar)
        + _frame(fac, rar, fw)
        + _art(fac, rar, art_uri, ax, aw)
        + _header(model, fac, rar)
        + _ribbon(model, fac)
        + _stats(model, fac)
        + _abilities(model, fac)
        + _footer(model, fac, rar, fw)
    )
    return (f'<svg xmlns="http://www.w3.org/2000/svg" '
            f'xmlns:xlink="http://www.w3.org/1999/xlink" '
            f'width="{W}" height="{H}" viewBox="0 0 {W} {H}" '
            f'role="img" aria-label="{_esc(model["name"])} — '
            f'{_esc(model["faction"])} {_esc(model["rarity"])} card">{body}</svg>')


def find_splash(slug: str) -> Path | None:
    """Prefer the flux A variant; fall back to any variant for this slug."""
    a = SPLASH_DIR / f"{slug}__A_flux.png"
    if a.exists():
        return a
    cands = sorted(SPLASH_DIR.glob(f"{slug}__*.png"))
    cands = [c for c in cands if "__hires" not in c.name]
    return cands[0] if cands else None


def rasterise(svg_path: Path, png_path: Path) -> bool:
    try:
        subprocess.run(
            ["rsvg-convert", "-w", str(W * RENDER_SCALE), "-h", str(H * RENDER_SCALE),
             "-o", str(png_path), str(svg_path)],
            check=True, capture_output=True,
        )
        return png_path.exists()
    except Exception as e:  # noqa: BLE001
        print(f"    ! raster failed for {svg_path.name}: {e}", file=sys.stderr)
        return False


FACTION_ORDER = ["Greek", "Roman", "Egyptian", "Norse",
                 "Ember", "Tide", "Verdant", "Umbral"]
RARITY_ORDER = {"Legendary": 0, "Epic": 1, "Rare": 2, "Common": 3}


def build_gallery(cards: list[dict]) -> str:
    groups: dict[str, list[dict]] = {}
    for c in cards:
        groups.setdefault(c["faction"], []).append(c)

    sections = []
    for fac_name in FACTION_ORDER + [f for f in groups if f not in FACTION_ORDER]:
        if fac_name not in groups:
            continue
        fac = faction_tokens(fac_name)
        items = sorted(groups[fac_name],
                       key=lambda c: (RARITY_ORDER.get(c["rarity"], 9), c["name"]))
        tiles = []
        for c in items:
            flag = ' <span class="warn">crop?</span>' if c.get("crop_warn") else ""
            tiles.append(
                f'<figure><img src="{c["slug"]}.png" alt="{_esc(c["name"])}" loading="lazy"/>'
                f'<figcaption><b>{_esc(c["name"])}</b><br/>'
                f'<span class="meta">{_esc(c["rarity"])} · cost {c["cost"]}</span>{flag}'
                f'</figcaption></figure>'
            )
        sections.append(
            f'<section>'
            f'<h2 style="border-color:{fac["primary"]};color:{fac["light"]}">'
            f'<span class="dot" style="background:{fac["primary"]}"></span>'
            f'{_esc(fac["label"])} <span class="count">{len(items)}</span></h2>'
            f'<div class="grid">{"".join(tiles)}</div></section>'
        )

    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Aeonrift · Ascendancy — Finished Cards ({len(cards)})</title>
<style>
  :root{{color-scheme:dark}}
  body{{margin:0;background:#08060d;color:#e9e3f5;
    font-family:'Georgia',system-ui,serif}}
  header.top{{padding:34px 40px 26px;border-bottom:1px solid #241c38;
    background:linear-gradient(180deg,#120c1e,#08060d)}}
  header.top h1{{margin:0;font-size:30px;letter-spacing:5px}}
  header.top p{{margin:8px 0 0;color:#9a8fbe;font-family:system-ui,sans-serif;
    font-size:14px;letter-spacing:1px}}
  section{{padding:14px 40px 30px}}
  h2{{font-size:17px;letter-spacing:3px;text-transform:uppercase;
    border-left:4px solid;padding:6px 0 6px 14px;margin:26px 0 18px;
    display:flex;align-items:center;gap:12px}}
  h2 .dot{{width:12px;height:12px;border-radius:50%;display:inline-block;
    box-shadow:0 0 12px currentColor}}
  h2 .count{{margin-left:auto;font-size:12px;color:#8a7fae;
    font-family:system-ui,sans-serif}}
  .grid{{display:grid;grid-template-columns:repeat(auto-fill,minmax(230px,1fr));
    gap:26px}}
  figure{{margin:0}}
  img{{width:100%;border-radius:16px;display:block;
    box-shadow:0 10px 30px rgba(0,0,0,.6);transition:transform .15s ease}}
  img:hover{{transform:translateY(-4px) scale(1.02)}}
  figcaption{{margin-top:10px;font-size:13px;text-align:center;line-height:1.5}}
  .meta{{color:#a89ccb;font-family:system-ui,sans-serif;font-size:12px}}
  .warn{{color:#ffb454;font-family:system-ui,sans-serif;font-size:11px;
    border:1px solid #7a5320;border-radius:8px;padding:1px 6px;margin-left:6px}}
</style></head><body>
<header class="top"><h1>AEONRIFT · ASCENDANCY</h1>
<p>{len(cards)} FINISHED CHAMPION CARDS · painterly splash + procedural faction/rarity frame system</p>
</header>
{''.join(sections)}
</body></html>"""


def main() -> int:
    ap = argparse.ArgumentParser(description="Composite finished Ascendancy cards.")
    ap.add_argument("--only", help="render a single slug (debug)")
    ap.add_argument("--keep-svg", action="store_true", help="retain intermediate SVGs")
    args = ap.parse_args()

    if not shutil.which("rsvg-convert"):
        print("FATAL: rsvg-convert not found (brew install librsvg).", file=sys.stderr)
        return 2

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    ART_STAGE.mkdir(parents=True, exist_ok=True)

    roster = parse_roster_meta((CHAMP_DIR / "roster_meta.ron").read_text(encoding="utf-8"))
    slugs = [p.stem for p in sorted(CHAMP_DIR.glob("*.ron")) if p.stem != "roster_meta"]
    if args.only:
        slugs = [s for s in slugs if s == args.only]

    cards: list[dict] = []
    made = 0
    missing: list[str] = []
    warns: list[tuple[str, float]] = []

    for slug in slugs:
        splash = find_splash(slug)
        if splash is None:
            missing.append(slug)
            print(f"  ! {slug}: no splash found in {SPLASH_DIR.name}", file=sys.stderr)
            continue

        model = load_champion(slug, CHAMP_DIR, roster)
        rar = rarity_tokens(model["rarity"])
        fw = round(rar["frame_w"] * 1.4)  # 13..21 across rarities
        ax = fw + 16
        aw = W - 2 * ax

        uri, report = art_data_uri(splash, aw, ART_H, ART_STAGE / f"{slug}.png")

        crop_warn = report["kept_v_frac"] < 0.60  # <60% of splash height survived
        if crop_warn:
            warns.append((slug, report["kept_v_frac"]))

        svg = build_card_svg(model, uri, fw, ax, aw)
        svg_path = OUT_DIR / f"{slug}.svg"
        svg_path.write_text(svg, encoding="utf-8")

        png_path = OUT_DIR / f"{slug}.png"
        ok = rasterise(svg_path, png_path)
        if not args.keep_svg:
            svg_path.unlink(missing_ok=True)
        if ok:
            made += 1
        model["crop_warn"] = crop_warn
        cards.append(model)
        print(f"  {model['faction']:>9} {model['rarity']:>9} {slug:<28} "
              f"cost {model['cost']}  kept {report['kept_v_frac']:.0%}"
              f"{'  [CROP?]' if crop_warn else ''}")

    (OUT_DIR / "GALLERY.html").write_text(build_gallery(cards), encoding="utf-8")

    print(f"\nDone: {made}/{len(slugs)} finished cards -> "
          f"{OUT_DIR.relative_to(AEON)}")
    print(f"      gallery -> {(OUT_DIR / 'GALLERY.html').relative_to(AEON)}")
    if missing:
        print(f"      MISSING splashes ({len(missing)}): {', '.join(missing)}")
    if warns:
        print(f"      crop review ({len(warns)}): "
              + ", ".join(f"{s} ({f:.0%})" for s, f in warns))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
