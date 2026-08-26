"""Assemble one polished trading-card SVG from a champion model.

Layout (600 x 840, a 5:7 trading-card ratio), top to bottom:

    +----------------------------------------------------+
    | (cost)     NAME                          (gem+pips) |   header
    |            the Title                                |
    |  +----------------------------------------------+  |
    |  |                ART SLOT                       |  |   sigil OR
    |  |        (faction sigil / AI illustration)     |  |   AI image
    |  +----------------------------------------------+  |
    |  [ FACTION ................... ARCHETYPE ]          |   ribbon
    |   HP 760   AD 40   ARM 33   RNG 5                   |   stats row
    |  +----------------------------------------------+  |
    |  | ABILITIES                                    |  |
    |  |  Spear of Peleus    Dash 13 · Deal 46 · Stun |  |   ability box
    |  |  Myrmidon Cleave    Deal 44 (+35% AD)   AoE  |  |  (+ sigil
    |  |  Immortal Rage      Heal 80            SELF  |  |   watermark)
    |  +----------------------------------------------+  |
    |  AEONRIFT · ASCENDANCY               LEGENDARY  *   |   footer
    +----------------------------------------------------+

Faction owns colour; rarity owns the metal/gem/ornament treatment. See
factions.py for the tokens. Pure string assembly -> valid standalone SVG.
"""

from __future__ import annotations

import html
from pathlib import Path

from factions import faction_tokens, rarity_tokens
from sigil import render_sigil

W, H = 600, 840


def _esc(s: str) -> str:
    return html.escape(str(s), quote=True)


# --- small icon glyphs (drawn at 0,0 in a 24x24 box, translated into place) ---

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


def _defs(fac: dict, rar: dict, slug: str) -> str:
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
  <radialGradient id="art" cx="0.5" cy="0.42" r="0.75">
    <stop offset="0" stop-color="{fac['art0']}"/>
    <stop offset="1" stop-color="{fac['art1']}"/>
  </radialGradient>
  <radialGradient id="gem" cx="0.35" cy="0.3" r="0.8">
    <stop offset="0" stop-color="{rar['gem_light']}"/>
    <stop offset="1" stop-color="{rar['gem']}"/>
  </radialGradient>
  <radialGradient id="cost" cx="0.4" cy="0.35" r="0.8">
    <stop offset="0" stop-color="{fac['light']}"/>
    <stop offset="1" stop-color="{fac['frame1']}"/>
  </radialGradient>
  <filter id="soft" x="-20%" y="-20%" width="140%" height="140%">
    <feGaussianBlur stdDeviation="6"/>
  </filter>
  <clipPath id="artclip"><rect x="46" y="128" width="508" height="344" rx="14"/></clipPath>
</defs>'''


def _frame(fac: dict, rar: dict) -> str:
    fw = rar["frame_w"]
    glow = ""
    if rar["glow"]:
        glow = (f'<rect x="6" y="6" width="{W-12}" height="{H-12}" rx="30" '
                f'fill="none" stroke="{rar["gem"]}" stroke-width="10" '
                f'opacity="0.55" filter="url(#soft)"/>')
    body = f'<rect x="4" y="4" width="{W-8}" height="{H-8}" rx="26" fill="url(#frame)"/>'
    inner = (f'<rect x="{fw}" y="{fw}" width="{W-2*fw}" height="{H-2*fw}" '
             f'rx="18" fill="url(#bg)"/>')
    band = (f'<rect x="{fw-3}" y="{fw-3}" width="{W-2*fw+6}" height="{H-2*fw+6}" '
            f'rx="20" fill="none" stroke="{rar["band"]}" stroke-width="1.5" opacity="0.9"/>')
    orn = _ornaments(fac, rar) if rar["ornaments"] else ""
    return glow + body + inner + band + orn


def _ornaments(fac: dict, rar: dict) -> str:
    """Corner filigree flourishes for Epic/Legendary."""
    c = rar["gem"]
    corners = [(28, 28, 1, 1), (W - 28, 28, -1, 1),
               (28, H - 28, 1, -1), (W - 28, H - 28, -1, -1)]
    out = []
    for cx, cy, sx, sy in corners:
        out.append(
            f'<path d="M{cx},{cy+22*sy} q0,{-22*sy} {22*sx},{-22*sy} '
            f'M{cx+6*sx},{cy+22*sy} q0,{-16*sy} {16*sx},{-16*sy}" '
            f'fill="none" stroke="{c}" stroke-width="2.5" opacity="0.85"/>'
            f'<circle cx="{cx}" cy="{cy}" r="3.5" fill="{c}"/>')
    return "".join(out)


def _header(m: dict, fac: dict, rar: dict) -> str:
    name = _esc(m["name"]).upper()
    title = _esc(m["title"])
    # cost badge (top-left)
    cost = (f'<g>'
            f'<circle cx="70" cy="78" r="40" fill="url(#cost)" stroke="{fac["frame1"]}" stroke-width="3"/>'
            f'<circle cx="70" cy="78" r="40" fill="none" stroke="{fac["light"]}" stroke-width="1" opacity="0.6"/>'
            f'<polygon points="{_diamond(70,50,7)}" fill="{fac["bg1"]}" opacity="0.85"/>'
            f'<text x="70" y="80" font-family="Georgia, serif" font-size="40" font-weight="700" '
            f'fill="{fac["bg1"]}" text-anchor="middle" dominant-baseline="central">{m["cost"]}</text>'
            f'</g>')
    # rarity gem (top-right) + rank pips
    gx, gy = 528, 66
    pips = "".join(
        f'<circle cx="{gx-15+i*10}" cy="{gy+30}" r="3.4" fill="{rar["gem"]}" '
        f'stroke="{fac["bg1"]}" stroke-width="0.8"/>'
        for i in range(rar["pips"])
    )
    gem = (f'<g>'
           f'<polygon points="{_diamond(gx,gy,22)}" fill="url(#gem)" '
           f'stroke="{fac["frame1"]}" stroke-width="2.5"/>'
           f'<polygon points="{_diamond(gx,gy,22)}" fill="none" stroke="{rar["gem_light"]}" '
           f'stroke-width="1" opacity="0.8"/>'
           f'<polygon points="{gx-22},{gy} {gx},{gy-22} {gx},{gy} " fill="{rar["gem_light"]}" opacity="0.35"/>'
           f'{pips}</g>')
    # name + title (centred between badges)
    name_txt = (f'<text x="{W/2+8}" y="60" font-family="Georgia, \'Times New Roman\', serif" '
                f'font-size="34" font-weight="700" letter-spacing="1.5" '
                f'fill="{fac["light"]}" text-anchor="middle" '
                f'style="paint-order:stroke;stroke:{fac["bg1"]};stroke-width:3px">{name}</text>')
    title_txt = (f'<text x="{W/2+8}" y="92" font-family="Georgia, serif" font-size="17" '
                 f'font-style="italic" fill="{fac["primary"]}" text-anchor="middle" '
                 f'letter-spacing="0.5">{title}</text>') if title else ""
    divider = (f'<line x1="120" y1="112" x2="480" y2="112" stroke="{fac["frame1"]}" '
               f'stroke-width="1.5" opacity="0.6"/>'
               f'<polygon points="{_diamond(300,112,5)}" fill="{fac["primary"]}"/>')
    return cost + gem + name_txt + title_txt + divider


def _diamond(cx: float, cy: float, r: float) -> str:
    return f"{cx},{cy-r} {cx+r},{cy} {cx},{cy+r} {cx-r},{cy}"


def _art_slot(m: dict, fac: dict, rar: dict, art_png: Path | None) -> str:
    x, y, w, h = 46, 128, 508, 344
    bg = f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="14" fill="url(#art)"/>'
    if art_png is not None and art_png.exists():
        # AI illustration present -> drop it into the framed slot (see
        # generate_illustrations.py for how these are produced).
        content = (f'<image x="{x}" y="{y}" width="{w}" height="{h}" '
                   f'href="{_esc(art_png.as_posix())}" preserveAspectRatio="xMidYMid slice" '
                   f'clip-path="url(#artclip)"/>')
    else:
        letter = _esc(m["name"][:1].upper())
        content = render_sigil(300, 296, 150, m["slug"], letter,
                               fac["motif"], fac["sigil0"], fac["sigil1"], fac["light"])
    bevel = (f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="14" fill="none" '
             f'stroke="{fac["frame1"]}" stroke-width="4"/>'
             f'<rect x="{x+4}" y="{y+4}" width="{w-8}" height="{h-8}" rx="11" fill="none" '
             f'stroke="{rar["band"]}" stroke-width="1.2" opacity="0.7"/>')
    rivets = "".join(
        f'<circle cx="{rx}" cy="{ry}" r="4" fill="{fac["frame0"]}" stroke="{fac["bg1"]}" stroke-width="1"/>'
        for rx, ry in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)]
    )
    return bg + content + bevel + rivets


def _ribbon(m: dict, fac: dict) -> str:
    y = 486
    left = _esc(fac["label"])
    right = _esc(m["archetype"]).upper()
    plate = (f'<path d="M60,{y} L540,{y} L528,{y+34} L72,{y+34} Z" '
             f'fill="url(#ribbon)" stroke="{fac["bg1"]}" stroke-width="1.5"/>')
    ftext = (f'<text x="86" y="{y+23}" font-family="Georgia, serif" font-size="15" '
             f'font-weight="700" letter-spacing="1.2" fill="{fac["bg1"]}">{left}</text>')
    atext = (f'<text x="514" y="{y+23}" font-family="Georgia, serif" font-size="15" '
             f'font-weight="700" letter-spacing="1.2" fill="{fac["bg1"]}" '
             f'text-anchor="end">{right}</text>')
    dot = f'<polygon points="{_diamond(300,y+17,7)}" fill="{fac["bg1"]}"/>'
    return plate + ftext + atext + dot


def _stats(m: dict, fac: dict) -> str:
    s = m["stats"]
    cells = [
        ("hp", "HP", _n(s.get("hp"))),
        ("ad", "AD", _n(s.get("ad"))),
        ("armor", "ARM", _n(s.get("armor"))),
        ("range", "RNG", _n(s.get("range"))),
    ]
    y = 534
    out = []
    for i, (icon, lbl, val) in enumerate(cells):
        cx = 78 + i * 148
        out.append(
            f'<rect x="{cx-58}" y="{y}" width="116" height="52" rx="10" '
            f'fill="{fac["bg1"]}" stroke="{fac["frame1"]}" stroke-width="1.5" opacity="0.92"/>'
            + _icon(icon, cx - 48, y + 14, fac["primary"], 1.0)
            + f'<text x="{cx-16}" y="{y+22}" font-family="sans-serif" font-size="11" '
              f'fill="{fac["primary"]}" font-weight="600" letter-spacing="1">{lbl}</text>'
            + f'<text x="{cx-16}" y="{y+42}" font-family="Georgia, serif" font-size="21" '
              f'font-weight="700" fill="{fac["light"]}">{val}</text>'
        )
    return "".join(out)


def _n(v) -> str:
    if v is None:
        return "-"
    f = float(v)
    return str(int(f)) if f == int(f) else f"{f:g}"


_TARGET_TAG = {
    "Area": "AOE", "Skillshot": "SKILL", "SelfCast": "SELF", "Target": "TARGET",
}


def _abilities(m: dict, fac: dict) -> str:
    x, y, w, h = 46, 606, 508, 196
    panel = (f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="14" '
             f'fill="{fac["bg1"]}" stroke="{fac["frame1"]}" stroke-width="2.5" opacity="0.96"/>')
    # sigil watermark behind the text (low opacity, faction motif)
    letter = _esc(m["name"][:1].upper())
    wm = (f'<g opacity="0.08" clip-path="url(#abclip)">'
          f'{render_sigil(300, y + h/2, 120, m["slug"], letter, fac["motif"], fac["primary"], fac["primary"], fac["primary"])}'
          f'</g>')
    wm_clip = f'<clipPath id="abclip"><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="14"/></clipPath>'
    header = (f'<text x="{x+20}" y="{y+26}" font-family="Georgia, serif" font-size="14" '
              f'font-weight="700" letter-spacing="2" fill="{fac["primary"]}">ABILITIES</text>'
              f'<line x1="{x+20}" y1="{y+34}" x2="{x+w-20}" y2="{y+34}" '
              f'stroke="{fac["frame1"]}" stroke-width="1" opacity="0.5"/>')
    rows = []
    abilities = m["abilities"][:3]
    row_h = (h - 48) / max(1, len(abilities))
    for i, ab in enumerate(abilities):
        ry = y + 44 + i * row_h
        aname = _esc(ab["name"])
        eff = " · ".join(ab["effects"]) if ab["effects"] else "Utility"
        tag = _TARGET_TAG.get(ab.get("targeting") or "", "")
        cost = ab.get("cost")
        chip = ""
        if tag:
            chip = (f'<rect x="{x+w-92}" y="{ry+2}" width="72" height="20" rx="10" '
                    f'fill="none" stroke="{fac["frame1"]}" stroke-width="1.2"/>'
                    f'<text x="{x+w-56}" y="{ry+16}" font-family="sans-serif" font-size="10" '
                    f'font-weight="700" letter-spacing="1" fill="{fac["primary"]}" '
                    f'text-anchor="middle">{tag}</text>')
        cost_txt = f'  ·  {int(cost)} mana' if cost else ""
        rows.append(
            f'<text x="{x+20}" y="{ry+15}" font-family="Georgia, serif" font-size="16" '
            f'font-weight="700" fill="{fac["light"]}">{aname}</text>'
            f'{chip}'
            f'<text x="{x+20}" y="{ry+35}" font-family="sans-serif" font-size="12.5" '
            f'fill="{fac["primary"]}" opacity="0.92">{_esc(eff)}{cost_txt}</text>'
        )
    return wm_clip + panel + wm + header + "".join(rows)


def _footer(m: dict, fac: dict, rar: dict) -> str:
    y = 822
    left = (f'<text x="46" y="{y}" font-family="Georgia, serif" font-size="12" '
            f'letter-spacing="2" fill="{fac["primary"]}" opacity="0.85">'
            f'AEONRIFT &#183; ASCENDANCY</text>')
    star = f'<polygon points="{_star_pts(540, y-4, 8, 3.4, 5)}" fill="{rar["gem"]}"/>'
    right = (f'<text x="524" y="{y}" font-family="Georgia, serif" font-size="12" '
             f'font-weight="700" letter-spacing="1.5" fill="{rar["gem"]}" '
             f'text-anchor="end">{_esc(rar["label"])}</text>')
    return left + right + star


def _star_pts(cx, cy, ro, ri, n):
    import math
    pts = []
    for i in range(n * 2):
        r = ro if i % 2 == 0 else ri
        a = math.radians(-90 + i * 180 / n)
        pts.append(f"{cx+r*math.cos(a):.1f},{cy+r*math.sin(a):.1f}")
    return " ".join(pts)


def build_card_svg(model: dict, art_png: Path | None = None) -> str:
    fac = faction_tokens(model["faction"])
    rar = rarity_tokens(model["rarity"])
    body = (
        _defs(fac, rar, model["slug"])
        + _frame(fac, rar)
        + _header(model, fac, rar)
        + _art_slot(model, fac, rar, art_png)
        + _ribbon(model, fac)
        + _stats(model, fac)
        + _abilities(model, fac)
        + _footer(model, fac, rar)
    )
    return (f'<svg xmlns="http://www.w3.org/2000/svg" '
            f'xmlns:xlink="http://www.w3.org/1999/xlink" '
            f'width="{W}" height="{H}" viewBox="0 0 {W} {H}" '
            f'role="img" aria-label="{_esc(model["name"])} — {_esc(model["faction"])} {_esc(model["rarity"])} card">'
            f'{body}</svg>')
