"""Procedural faction SIGIL — the designed emblem that fills the art slot when no
AI illustration is present.

This is the single most important reason the card set does NOT look like
programmer-art: the art slot is never empty and never random noise. Each champion
gets a *deterministic, designed* emblem = a faction motif ring + a central
geometric glyph + the champion's monogram, seeded by the slug so it is stable and
unique but always on-theme. It reads as intentional heraldry, not a placeholder.

Everything here is pure geometry -> SVG string. No randomness at runtime beyond a
hash of the slug (deterministic across runs / machines).
"""

from __future__ import annotations

import hashlib
import math


def _seed(slug: str) -> int:
    """Stable integer seed from the champion slug (deterministic everywhere)."""
    return int(hashlib.sha1(slug.encode("utf-8")).hexdigest(), 16)


def _pt(cx: float, cy: float, r: float, ang_deg: float) -> tuple[float, float]:
    a = math.radians(ang_deg)
    return (cx + r * math.cos(a), cy + r * math.sin(a))


def _poly(cx: float, cy: float, r: float, n: int, rot: float = -90.0) -> str:
    pts = [_pt(cx, cy, r, rot + i * 360.0 / n) for i in range(n)]
    return " ".join(f"{x:.1f},{y:.1f}" for x, y in pts)


def _star(cx: float, cy: float, r_out: float, r_in: float, n: int, rot: float = -90.0) -> str:
    pts = []
    for i in range(n * 2):
        r = r_out if i % 2 == 0 else r_in
        pts.append(_pt(cx, cy, r, rot + i * 180.0 / n))
    return " ".join(f"{x:.1f},{y:.1f}" for x, y in pts)


# --- per-faction motif rings ------------------------------------------------
# Each returns an SVG fragment drawn around (cx, cy) at radius R. They share a
# common visual grammar (a rim + a repeated motif + radiating spokes) so the
# eight factions feel like one heraldic system.

def _greek(cx, cy, R, c0, c1, seed):
    # laurel-ish twin rings + Greek-key ticks + central column/omega
    ticks = "".join(
        f'<line x1="{_pt(cx,cy,R*0.86,a)[0]:.1f}" y1="{_pt(cx,cy,R*0.86,a)[1]:.1f}" '
        f'x2="{_pt(cx,cy,R*0.98,a)[0]:.1f}" y2="{_pt(cx,cy,R*0.98,a)[1]:.1f}" '
        f'stroke="{c0}" stroke-width="3"/>'
        for a in range(0, 360, 15)
    )
    col = (f'<rect x="{cx-8:.1f}" y="{cy-R*0.5:.1f}" width="16" height="{R:.1f}" rx="3" '
           f'fill="none" stroke="{c1}" stroke-width="4"/>'
           f'<rect x="{cx-R*0.28:.1f}" y="{cy+R*0.42:.1f}" width="{R*0.56:.1f}" height="10" rx="2" fill="{c1}"/>'
           f'<rect x="{cx-R*0.28:.1f}" y="{cy-R*0.5:.1f}" width="{R*0.56:.1f}" height="10" rx="2" fill="{c1}"/>')
    return (f'<circle cx="{cx}" cy="{cy}" r="{R*0.92:.1f}" fill="none" stroke="{c0}" stroke-width="3"/>'
            f'<circle cx="{cx}" cy="{cy}" r="{R*0.82:.1f}" fill="none" stroke="{c1}" stroke-width="1.5"/>'
            f'{ticks}{col}')


def _norse(cx, cy, R, c0, c1, seed):
    # Valknut (three interlocked triangles) + angular runic rim
    tris = ""
    for k in range(3):
        rot = -90 + k * 120
        tris += f'<polygon points="{_poly(cx,cy,R*0.62,3,rot)}" fill="none" stroke="{c0}" stroke-width="4"/>'
    rim = f'<polygon points="{_poly(cx,cy,R*0.95,8)}" fill="none" stroke="{c1}" stroke-width="3"/>'
    runes = "".join(
        f'<line x1="{_pt(cx,cy,R*0.7,a)[0]:.1f}" y1="{_pt(cx,cy,R*0.7,a)[1]:.1f}" '
        f'x2="{_pt(cx,cy,R*0.88,a)[0]:.1f}" y2="{_pt(cx,cy,R*0.88,a)[1]:.1f}" '
        f'stroke="{c1}" stroke-width="2.5"/>'
        for a in range(0, 360, 45)
    )
    return rim + runes + tris


def _roman(cx, cy, R, c0, c1, seed):
    # eagle spread via nested chevrons + laurel rim + standard bars
    chev = ""
    for i in range(4):
        rr = R * (0.3 + i * 0.16)
        lx, ly = _pt(cx, cy, rr, 200)
        mx, my = cx, cy - rr * 0.5
        rx, ry = _pt(cx, cy, rr, -20)
        chev += (f'<polyline points="{lx:.1f},{ly:.1f} {mx:.1f},{my:.1f} {rx:.1f},{ry:.1f}" '
                 f'fill="none" stroke="{c0}" stroke-width="3"/>')
    rim = f'<circle cx="{cx}" cy="{cy}" r="{R*0.93:.1f}" fill="none" stroke="{c1}" stroke-width="3"/>'
    bars = (f'<rect x="{cx-3:.1f}" y="{cy-R*0.9:.1f}" width="6" height="{R*0.35:.1f}" fill="{c0}"/>'
            f'<polygon points="{_poly(cx,cy-R*0.55,10,3,-90)}" fill="{c0}"/>')
    return rim + chev + bars


def _egyptian(cx, cy, R, c0, c1, seed):
    # sun disc + ankh + radiating rays (turquoise accent from c1)
    rays = "".join(
        f'<polygon points="{_pt(cx,cy,R*0.72,a)[0]:.1f},{_pt(cx,cy,R*0.72,a)[1]:.1f} '
        f'{_pt(cx,cy,R*0.98,a-3)[0]:.1f},{_pt(cx,cy,R*0.98,a-3)[1]:.1f} '
        f'{_pt(cx,cy,R*0.98,a+3)[0]:.1f},{_pt(cx,cy,R*0.98,a+3)[1]:.1f}" fill="{c0}"/>'
        for a in range(0, 360, 30)
    )
    disc = f'<circle cx="{cx}" cy="{cy-R*0.15:.1f}" r="{R*0.5:.1f}" fill="none" stroke="{c0}" stroke-width="4"/>'
    ankh = (f'<circle cx="{cx}" cy="{cy-R*0.3:.1f}" r="{R*0.2:.1f}" fill="none" stroke="{c1}" stroke-width="4"/>'
            f'<line x1="{cx}" y1="{cy-R*0.1:.1f}" x2="{cx}" y2="{cy+R*0.55:.1f}" stroke="{c1}" stroke-width="4"/>'
            f'<line x1="{cx-R*0.28:.1f}" y1="{cy+R*0.12:.1f}" x2="{cx+R*0.28:.1f}" y2="{cy+R*0.12:.1f}" stroke="{c1}" stroke-width="4"/>')
    return rays + disc + ankh


def _ember(cx, cy, R, c0, c1, seed):
    # radiating flame tongues + molten core
    flames = ""
    n = 12
    for i in range(n):
        a = i * 360 / n
        bx, by = _pt(cx, cy, R * 0.35, a)
        tx, ty = _pt(cx, cy, R * 0.98, a)
        lx, ly = _pt(cx, cy, R * 0.6, a - 9)
        rx, ry = _pt(cx, cy, R * 0.6, a + 9)
        flames += (f'<path d="M{bx:.1f},{by:.1f} Q{lx:.1f},{ly:.1f} {tx:.1f},{ty:.1f} '
                   f'Q{rx:.1f},{ry:.1f} {bx:.1f},{by:.1f} Z" fill="{c0}" opacity="0.85"/>')
    core = f'<circle cx="{cx}" cy="{cy}" r="{R*0.34:.1f}" fill="{c1}"/>' \
           f'<circle cx="{cx}" cy="{cy}" r="{R*0.2:.1f}" fill="{c0}"/>'
    return flames + core


def _tide(cx, cy, R, c0, c1, seed):
    # concentric wave arcs + trident + bubbles
    waves = ""
    for i in range(3):
        rr = R * (0.55 + i * 0.18)
        waves += (f'<path d="M{cx-rr:.1f},{cy} Q{cx-rr*0.5:.1f},{cy-rr*0.35:.1f} {cx:.1f},{cy} '
                  f'T{cx+rr:.1f},{cy}" fill="none" stroke="{c0}" stroke-width="3"/>')
    tri = (f'<line x1="{cx}" y1="{cy-R*0.7:.1f}" x2="{cx}" y2="{cy+R*0.7:.1f}" stroke="{c1}" stroke-width="4"/>'
           f'<path d="M{cx-R*0.28:.1f},{cy-R*0.4:.1f} L{cx-R*0.28:.1f},{cy-R*0.7:.1f} '
           f'M{cx+R*0.28:.1f},{cy-R*0.4:.1f} L{cx+R*0.28:.1f},{cy-R*0.7:.1f} '
           f'M{cx:.1f},{cy-R*0.45:.1f} L{cx:.1f},{cy-R*0.8:.1f}" stroke="{c1}" stroke-width="4" fill="none"/>')
    rim = f'<circle cx="{cx}" cy="{cy}" r="{R*0.95:.1f}" fill="none" stroke="{c1}" stroke-width="2.5"/>'
    return rim + waves + tri


def _verdant(cx, cy, R, c0, c1, seed):
    # radial leaves + seed-of-life hex ring
    leaves = ""
    n = 8
    for i in range(n):
        a = i * 360 / n
        bx, by = _pt(cx, cy, R * 0.2, a)
        tx, ty = _pt(cx, cy, R * 0.95, a)
        lx, ly = _pt(cx, cy, R * 0.55, a - 14)
        rx, ry = _pt(cx, cy, R * 0.55, a + 14)
        leaves += (f'<path d="M{bx:.1f},{by:.1f} Q{lx:.1f},{ly:.1f} {tx:.1f},{ty:.1f} '
                   f'Q{rx:.1f},{ry:.1f} {bx:.1f},{by:.1f} Z" fill="none" stroke="{c0}" stroke-width="2.5"/>')
    ring = "".join(
        f'<circle cx="{_pt(cx,cy,R*0.42,a)[0]:.1f}" cy="{_pt(cx,cy,R*0.42,a)[1]:.1f}" '
        f'r="{R*0.42:.1f}" fill="none" stroke="{c1}" stroke-width="1.2" opacity="0.5"/>'
        for a in range(0, 360, 60)
    )
    return ring + leaves


def _umbral(cx, cy, R, c0, c1, seed):
    # eldritch many-pointed star + orbiting void nodes
    pts = 7 + (seed % 3)  # 7..9 points, seeded
    star = f'<polygon points="{_star(cx,cy,R*0.95,R*0.42,pts)}" fill="none" stroke="{c0}" stroke-width="3"/>'
    inner = f'<polygon points="{_star(cx,cy,R*0.6,R*0.26,pts,-90+25)}" fill="none" stroke="{c1}" stroke-width="1.5"/>'
    nodes = "".join(
        f'<circle cx="{_pt(cx,cy,R*0.8,a)[0]:.1f}" cy="{_pt(cx,cy,R*0.8,a)[1]:.1f}" r="4" fill="{c1}"/>'
        for a in range(0, 360, 360 // pts)
    )
    return star + inner + nodes


_MOTIFS = {
    "greek": _greek, "norse": _norse, "roman": _roman, "egyptian": _egyptian,
    "ember": _ember, "tide": _tide, "verdant": _verdant, "umbral": _umbral,
}


def render_sigil(cx: float, cy: float, R: float, slug: str, letter: str,
                 motif: str, sigil0: str, sigil1: str, light: str) -> str:
    """Compose the full art-slot emblem: seeded burst + faction motif + monogram."""
    seed = _seed(slug)
    draw = _MOTIFS.get(motif, _umbral)

    # deterministic radial burst behind the motif for per-champion uniqueness
    spokes = 18 + (seed % 10)
    burst = "".join(
        f'<line x1="{cx}" y1="{cy}" x2="{_pt(cx,cy,R*1.02,i*360/spokes)[0]:.1f}" '
        f'y2="{_pt(cx,cy,R*1.02,i*360/spokes)[1]:.1f}" stroke="{sigil1}" '
        f'stroke-width="1" opacity="0.18"/>'
        for i in range(spokes)
    )

    motif_svg = draw(cx, cy, R, sigil0, sigil1, seed)

    # champion monogram (first initial) — the personal mark inside the crest
    mono = (f'<text x="{cx}" y="{cy}" font-family="Georgia, \'Times New Roman\', serif" '
            f'font-size="{R*0.62:.0f}" font-weight="700" fill="{light}" '
            f'text-anchor="middle" dominant-baseline="central" '
            f'opacity="0.92" style="paint-order:stroke;stroke:{sigil1};stroke-width:2px">'
            f'{letter}</text>')

    return f'<g>{burst}{motif_svg}{mono}</g>'
