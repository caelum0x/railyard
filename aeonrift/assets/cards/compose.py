"""Composite framed champion cards from Flux illustrations + champion data (Pillow).

Fully headless: reads assets/cards/art/<slug>.png, writes assets/cards/final/<slug>.png.
Cohesive frame across all 48: faction-tinted border, rarity gem/banner, elixir cost
badge, name + title typography, a hp/ad/armor stat row, and a signature-ability line.
"""
from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

from champions import load_all, Champion

HERE = Path(__file__).resolve().parent
ART = HERE / "art"
FINAL = HERE / "final"

# Card geometry (portrait, TCG proportions).
CW, CH = 744, 1040
MARGIN = 26          # outer frame thickness
ART_TOP = 150        # where the art window starts
ART_H = 560          # art window height
CORNER = 34

# macOS system fonts.
F_NAME = "/System/Library/Fonts/Supplemental/Copperplate.ttc"
F_SERIF = "/System/Library/Fonts/Supplemental/Didot.ttc"
F_SANS = "/System/Library/Fonts/HelveticaNeue.ttc"
F_SANS_B = "/System/Library/Fonts/Supplemental/Futura.ttc"

INK = (22, 20, 28)
PARCH = (28, 26, 34)      # dark card base
PARCH2 = (16, 15, 20)


def _hex(h: str) -> tuple[int, int, int]:
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def _font(path: str, size: int, index: int = 0) -> ImageFont.FreeTypeFont:
    try:
        return ImageFont.truetype(path, size, index=index)
    except Exception:
        return ImageFont.load_default()


def _rounded(draw, box, radius, fill=None, outline=None, width=1):
    draw.rounded_rectangle(box, radius=radius, fill=fill, outline=outline, width=width)


def _fit_text(draw, text, font_path, max_w, start, minimum, index=0):
    size = start
    while size > minimum:
        f = _font(font_path, size, index)
        if draw.textlength(text, font=f) <= max_w:
            return f
        size -= 2
    return _font(font_path, minimum, index)


def _vgrad(w: int, h: int, top: tuple, bot: tuple) -> Image.Image:
    base = Image.new("RGB", (w, h), top)
    px = base.load()
    for y in range(h):
        t = y / max(1, h - 1)
        c = tuple(int(top[i] + (bot[i] - top[i]) * t) for i in range(3))
        for x in range(w):
            px[x, y] = c
    return base


def _cover(img: Image.Image, w: int, h: int, y_bias: float = 0.18) -> Image.Image:
    """Scale + crop to fill w x h. y_bias<0.5 keeps more of the top (faces)."""
    iw, ih = img.size
    scale = max(w / iw, h / ih)
    nw, nh = int(iw * scale + 0.5), int(ih * scale + 0.5)
    img = img.resize((nw, nh), Image.LANCZOS)
    left = (nw - w) // 2
    top = int((nh - h) * y_bias)
    return img.crop((left, top, left + w, top + h))


def _mix(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))


def compose(champ: Champion) -> Image.Image | None:
    art_path = ART / f"{champ.slug}.png"
    if not art_path.exists():
        return None
    accent = _hex(champ.accent)
    rarity_accent = _hex(champ.rarity_accent)
    gem = _hex(champ.gem)

    card = Image.new("RGB", (CW, CH), PARCH2)
    draw = ImageDraw.Draw(card)

    # --- Base card body with faction-tinted vertical gradient ---
    body_top = _mix(PARCH, accent, 0.10)
    body_bot = PARCH2
    body = _vgrad(CW, CH, body_top, body_bot)
    mask = Image.new("L", (CW, CH), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, CW - 1, CH - 1], radius=CORNER, fill=255)
    card.paste(body, (0, 0), mask)
    draw = ImageDraw.Draw(card)

    # --- Outer frame: faction accent, double stroke ---
    _rounded(draw, [4, 4, CW - 5, CH - 5], CORNER, outline=accent, width=6)
    inner_col = _mix(accent, (255, 255, 255), 0.25)
    _rounded(draw, [12, 12, CW - 13, CH - 13], CORNER - 6, outline=_mix(inner_col, PARCH2, 0.4), width=2)

    # --- Art window ---
    ax0, ax1 = MARGIN + 8, CW - MARGIN - 8
    ay0, ay1 = ART_TOP, ART_TOP + ART_H
    art_img = Image.open(art_path).convert("RGB")
    art_filled = _cover(art_img, ax1 - ax0, ay1 - ay0)
    art_mask = Image.new("L", (ax1 - ax0, ay1 - ay0), 0)
    ImageDraw.Draw(art_mask).rounded_rectangle(
        [0, 0, ax1 - ax0 - 1, ay1 - ay0 - 1], radius=18, fill=255
    )
    card.paste(art_filled, (ax0, ay0), art_mask)
    # subtle inner shadow at bottom of art for text legibility of banner overlap
    _rounded(draw, [ax0, ay0, ax1, ay1], 18, outline=_mix(accent, (0, 0, 0), 0.3), width=4)

    # --- Top banner: NAME + TITLE ---
    name = champ.name.upper()
    fname = _fit_text(draw, name, F_NAME, CW - 2 * MARGIN - 120, 52, 26, index=2)
    nw = draw.textlength(name, font=fname)
    ny = 46
    # engraved look: dark shadow + accent highlight
    draw.text(((CW - nw) / 2 + 2, ny + 2), name, font=fname, fill=(0, 0, 0))
    draw.text(((CW - nw) / 2, ny), name, font=fname, fill=_mix(accent, (255, 255, 255), 0.55))
    if champ.title:
        title = champ.title.upper()
        ftitle = _fit_text(draw, title, F_SERIF, CW - 2 * MARGIN - 140, 26, 14, index=0)
        tw = draw.textlength(title, font=ftitle)
        draw.text(((CW - tw) / 2, ny + fname.size + 6), title, font=ftitle,
                  fill=_mix(accent, (230, 230, 230), 0.6))

    # --- Elixir COST badge (top-left gem) ---
    bd = 92
    bx, by = 20, 18
    badge = Image.new("RGBA", (bd, bd), (0, 0, 0, 0))
    bdr = ImageDraw.Draw(badge)
    bdr.ellipse([4, 4, bd - 4, bd - 4], fill=_mix(accent, (0, 0, 0), 0.15) + (255,),
                outline=(255, 255, 255), width=3)
    bdr.ellipse([16, 12, bd - 16, bd - 30], fill=_mix(accent, (255, 255, 255), 0.4) + (110,))
    card.paste(badge, (bx, by), badge)
    fcost = _font(F_SANS_B, 46, index=0)
    ct = str(champ.cost)
    cw = draw.textlength(ct, font=fcost)
    draw.text((bx + bd / 2 - cw / 2 + 1, by + bd / 2 - 30 + 1), ct, font=fcost, fill=(0, 0, 0))
    draw.text((bx + bd / 2 - cw / 2, by + bd / 2 - 30), ct, font=fcost, fill=(255, 255, 255))

    # --- Rarity gem (top-right) ---
    gd = 46
    gx, gy = CW - 20 - gd, 30
    pts = [(gx + gd / 2, gy), (gx + gd, gy + gd / 2),
           (gx + gd / 2, gy + gd), (gx, gy + gd / 2)]
    draw.polygon(pts, fill=gem, outline=(255, 255, 255))
    draw.polygon([(gx + gd / 2, gy + 6), (gx + gd - 8, gy + gd / 2),
                  (gx + gd / 2, gy + gd - 6), (gx + 8, gy + gd / 2)],
                 outline=_mix(gem, (255, 255, 255), 0.6))

    # --- Info panel below art ---
    py0 = ay1 + 18
    _rounded(draw, [MARGIN + 8, py0, CW - MARGIN - 8, CH - MARGIN - 8], 16,
             fill=_mix(PARCH2, accent, 0.06), outline=_mix(accent, PARCH2, 0.5), width=2)

    # Faction + archetype + rarity ribbon
    fmeta = _font(F_SANS_B, 20, index=1)
    ribbon = f"{champ.faction.upper()}   ·   {champ.archetype.upper()}   ·   {champ.rarity.upper()}"
    rw = draw.textlength(ribbon, font=fmeta)
    draw.text(((CW - rw) / 2, py0 + 16), ribbon, font=fmeta, fill=_mix(accent, (255, 255, 255), 0.5))
    draw.line([MARGIN + 40, py0 + 52, CW - MARGIN - 40, py0 + 52],
              fill=_mix(accent, PARCH2, 0.4), width=2)

    # --- STATS row (hp / ad / armor) ---
    stats = [("HP", champ.hp, (208, 92, 92)),
             ("AD", champ.ad, (222, 170, 80)),
             ("ARM", champ.armor, (120, 170, 214))]
    sy = py0 + 70
    col_w = (CW - 2 * (MARGIN + 24)) / 3
    flbl = _font(F_SANS_B, 19, index=1)
    fval = _font(F_SANS_B, 40, index=1)
    for i, (lbl, val, col) in enumerate(stats):
        cx = MARGIN + 24 + col_w * (i + 0.5)
        vs = str(val)
        vw = draw.textlength(vs, font=fval)
        draw.text((cx - vw / 2, sy), vs, font=fval, fill=col)
        lw = draw.textlength(lbl, font=flbl)
        draw.text((cx - lw / 2, sy + 46), lbl, font=flbl, fill=(150, 150, 160))
        if i < 2:
            dx = MARGIN + 24 + col_w * (i + 1)
            draw.line([dx, sy + 4, dx, sy + 64], fill=_mix(accent, PARCH2, 0.5), width=1)

    # --- ABILITY line ---
    aby = sy + 92
    fab_n = _font(F_SANS_B, 22, index=1)
    an = champ.ability_name
    anw = draw.textlength(an, font=fab_n)
    draw.text(((CW - anw) / 2, aby), an, font=fab_n, fill=_mix(accent, (255, 255, 255), 0.65))
    if champ.ability_line:
        fab = _font(F_SANS, 20, index=0)
        al = champ.ability_line
        alw = draw.textlength(al, font=fab)
        draw.text(((CW - alw) / 2, aby + 30), al, font=fab, fill=(180, 180, 190))

    return card


def main() -> int:
    FINAL.mkdir(parents=True, exist_ok=True)
    champs = load_all()
    made, missing = [], []
    for c in champs:
        img = compose(c)
        if img is None:
            missing.append(c.slug)
            continue
        img.save(FINAL / f"{c.slug}.png")
        made.append(c.slug)
    print(f"composited: {len(made)}   missing-art: {len(missing)}")
    if missing:
        print("  missing:", ", ".join(missing))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
