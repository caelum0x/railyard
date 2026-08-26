"""Background-matte pre-pass for Hunyuan3D-2.1 reference images (FULLY HEADLESS, LOCAL).

Hunyuan3D reconstructs the bright BACKGROUND of a concept `reference.png` as flat
white/colored base geometry under the character. To eliminate that dominant artifact we
matte the reference first: remove the background with `rembg` (u2net) so only the subject
survives, then composite the cutout onto a plain solid neutral mid-gray (#808080) so
Hunyuan's own segmentation has a single clean subject on a plain background to lock onto.

  * Input : <SRC>/<slug>/reference.png                (never modified)
  * Output: <SRC>/<slug>/reference.matte.png          (subject on #808080)

Idempotent: an existing matte is skipped unless --force. Run with the rembg venv:
    .venv-matte/bin/python matte.py [slug ...] [--force]
"""
from __future__ import annotations

import sys
import time
from pathlib import Path

from PIL import Image
from rembg import new_session, remove

SRC_BASE = Path("/Users/arhansubasi/products/rail-3d/aeonrift/assets/champions")
NEUTRAL = (128, 128, 128)  # mid-gray ~#808080

# 26 never-generated (credit ran out at champion 22) + 4 QA reverts to TRELLIS.
NEVER_GENERATED = [
    "mercury", "mosscloak", "neptune", "nerreth", "nyxralla", "odin", "petalsong",
    "pyromancer", "quorvane", "ra-radiant-dawn", "sentinel", "set-lord-of-chaos",
    "sporeveil", "thor", "thornquill", "toran", "tyr", "ullr", "vanguard", "vashka",
    "veyl", "vulcan", "xelphine", "ysmera", "zeryth", "zeus",
]
REVERTS = ["bastet-claws-of-the-hunt", "gorehoof", "lumisel", "medusa"]
DEFAULT_TARGETS = NEVER_GENERATED + REVERTS


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def matte_one(session, slug: str, force: bool) -> str:
    cdir = SRC_BASE / slug
    ref = cdir / "reference.png"
    out = cdir / "reference.matte.png"
    if not ref.is_file():
        return f"{slug}: SKIP (no reference.png)"
    if out.is_file() and not force:
        return f"{slug}: skip (matte exists)"

    src = Image.open(ref).convert("RGBA")
    # rembg -> subject on transparent alpha
    cut = remove(src, session=session)
    if cut.mode != "RGBA":
        cut = cut.convert("RGBA")
    # Composite cutout over solid neutral gray so Hunyuan sees a single clean subject.
    bg = Image.new("RGBA", cut.size, NEUTRAL + (255,))
    flat = Image.alpha_composite(bg, cut).convert("RGB")
    flat.save(out, "PNG")
    # crude subject coverage metric for the log (alpha > 16)
    alpha = cut.split()[-1]
    px = alpha.getdata()
    cover = sum(1 for a in px if a > 16) / max(len(px), 1)
    return f"{slug}: matted -> {out.name} ({flat.size[0]}x{flat.size[1]}, subject~{cover*100:.0f}%)"


def main() -> int:
    force = "--force" in sys.argv
    argsel = [a for a in sys.argv[1:] if not a.startswith("-")]
    targets = argsel or DEFAULT_TARGETS
    log(f"matte pre-pass: {len(targets)} champions (force={force})")
    session = new_session("u2net")
    for slug in targets:
        try:
            log("  " + matte_one(session, slug, force))
        except Exception as e:  # noqa: BLE001
            log(f"  {slug}: ERROR {type(e).__name__}: {str(e)[:160]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
