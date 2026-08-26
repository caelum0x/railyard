"""Matte-v2 pre-pass — TIGHTER cutout than v1 to kill residual white base geometry.

v1 matte (rembg u2net -> composite on solid #808080) left residual white/colored
geometry: a faint 1-3px halo of background survived the coarse u2net mask and Hunyuan
reconstructed it as an untextured white base-blob / plate / edge-fringe.

v2 fixes that with a TIGHTER alpha:
  * rembg u2net WITH alpha matting (alpha_matting=True) for a soft, accurate trimap edge,
  * then ERODE the alpha a few px (scipy grey_erosion) to pull the mask *inside* the
    subject silhouette so the halo fringe is cut away entirely,
  * then a light feather (small gaussian) so the eroded edge is not aliased.

For each target we emit TWO candidate references so Hunyuan3D-2.1's reconstruction can be
compared and the background that yields the LEAST base geometry chosen:
  * reference.matte2.png       — subject on a FULLY TRANSPARENT background (RGBA)
  * reference.matte2_gray.png  — subject composited on solid neutral #808080 (RGB)

Input : <SRC>/<slug>/reference.png                 (never modified)
Idempotent: existing outputs are skipped unless --force.
Run with the rembg venv (python 3.11): .venv-matte/bin/python matte_v2.py [slug ...] [--force]
"""
from __future__ import annotations

import sys
import time
from pathlib import Path

import numpy as np
from PIL import Image
from rembg import new_session, remove
from scipy import ndimage

SRC_BASE = Path("/Users/arhansubasi/products/rail-matte/aeonrift/assets/champions")
NEUTRAL = (128, 128, 128)  # mid-gray ~#808080

# The 11 matte-fixable flagged Hunyuan champions (white/colored base-blob/plate/shard/fringe).
TARGETS = [
    "cyranth", "kaeza", "korrathil", "mars", "veyl", "xelphine",
    "zeryth", "nyxralla", "petalsong", "artemis", "vulcan",
]

# Erode the alpha inward by this many px to cut the halo fringe, then feather.
ERODE_PX = 2
FEATHER_SIGMA = 0.8
# rembg alpha-matting trimap thresholds (tighter than defaults 240/10/10).
AM_FG = 240   # >= this alpha -> definite foreground
AM_BG = 15    # <= this alpha -> definite background
AM_ERODE = 11  # rembg's internal trimap erosion
# Background-colour key: a pixel rembg KEPT is re-cut if it is simultaneously
#   * close to the sampled flat background colour (RGB distance), AND
#   * bright (high HSV value), AND
#   * desaturated (low HSV saturation).
# All three protect saturated / dark character detail; only flat pale bg pockets match.
BG_DIST = 42.0
BG_VAL = 0.80
BG_SAT = 0.20
# Universal near-pure-white flat test (catches bright white blobs that differ from a
# non-white border, e.g. a gray-background concept with a white base-blob in the cloth).
WHITE_VAL = 0.90
WHITE_SAT = 0.10


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def sample_bg_color(rgb: np.ndarray, frame: int = 12) -> np.ndarray:
    """Median colour of the outer border frame — the flat concept background."""
    h, w, _ = rgb.shape
    edge = np.concatenate([
        rgb[:frame, :, :].reshape(-1, 3),
        rgb[-frame:, :, :].reshape(-1, 3),
        rgb[:, :frame, :].reshape(-1, 3),
        rgb[:, -frame:, :].reshape(-1, 3),
    ], axis=0)
    return np.median(edge, axis=0)


def bg_pocket_mask(rgb: np.ndarray, bg: np.ndarray) -> np.ndarray:
    """Boolean mask of flat background-coloured pixels (bright, desaturated, bg-close)."""
    f = rgb.astype(np.float32)
    dist = np.sqrt(((f - bg[None, None, :]) ** 2).sum(axis=2))
    mx = f.max(axis=2)
    mn = f.min(axis=2)
    val = mx / 255.0
    sat = np.where(mx > 0, (mx - mn) / np.maximum(mx, 1.0), 0.0)
    bg_match = (dist < BG_DIST) & (val > BG_VAL) & (sat < BG_SAT)
    near_white = (val > WHITE_VAL) & (sat < WHITE_SAT)
    return bg_match | near_white


def tighten_alpha(a: np.ndarray) -> np.ndarray:
    """Erode the alpha inward a few px then feather to remove the halo fringe."""
    size = 2 * ERODE_PX + 1
    eroded = ndimage.grey_erosion(a, size=(size, size))
    feathered = ndimage.gaussian_filter(eroded, sigma=FEATHER_SIGMA)
    return np.clip(feathered, 0.0, 1.0)


def coverage(a: np.ndarray) -> float:
    return float((a > 0.06).mean())


def matte_one(session, slug: str, force: bool) -> str:
    cdir = SRC_BASE / slug
    ref = cdir / "reference.png"
    out_t = cdir / "reference.matte2.png"
    out_g = cdir / "reference.matte2_gray.png"
    if not ref.is_file():
        return f"{slug}: SKIP (no reference.png)"
    if out_t.is_file() and out_g.is_file() and not force:
        return f"{slug}: skip (matte2 exists)"

    src = Image.open(ref).convert("RGBA")
    cut = remove(
        src,
        session=session,
        alpha_matting=True,
        alpha_matting_foreground_threshold=AM_FG,
        alpha_matting_background_threshold=AM_BG,
        alpha_matting_erode_size=AM_ERODE,
    )
    if cut.mode != "RGBA":
        cut = cut.convert("RGBA")

    r, g, b, a_img = cut.split()
    rgb = np.asarray(src.convert("RGB"))
    a0 = np.asarray(a_img, dtype=np.float32) / 255.0
    cov0 = coverage(a0)

    # 1) Knock out flat background-coloured pockets rembg wrongly kept (interior white
    #    holes inside bow-loops / trailing cloth -> Hunyuan's white base-plane source).
    bg = sample_bg_color(rgb)
    pocket = bg_pocket_mask(rgb, bg)
    a1 = np.where(pocket, 0.0, a0)
    cov_pocket = float(pocket.mean())

    # 2) Erode + feather to cut the residual halo fringe.
    a = tighten_alpha(a1)
    cov1 = coverage(a)

    a_final = Image.fromarray((a * 255.0 + 0.5).astype(np.uint8), mode="L")
    tight = Image.merge("RGBA", (r, g, b, a_final))

    # Transparent-background variant (RGBA, eroded alpha).
    tight.save(out_t, "PNG")

    # Solid neutral-gray variant (composite eroded cutout over #808080).
    bg_img = Image.new("RGBA", tight.size, NEUTRAL + (255,))
    flat = Image.alpha_composite(bg_img, tight).convert("RGB")
    flat.save(out_g, "PNG")

    return (f"{slug}: matte2 {tight.size[0]}x{tight.size[1]} bg~rgb{tuple(int(v) for v in bg)} "
            f"cover {cov0*100:.0f}%->{cov1*100:.0f}% "
            f"(pockets cut {cov_pocket*100:.1f}%, eroded {ERODE_PX}px) "
            f"-> {out_t.name} + {out_g.name}")


def main() -> int:
    force = "--force" in sys.argv
    argsel = [a for a in sys.argv[1:] if not a.startswith("-")]
    targets = argsel or TARGETS
    log(f"matte-v2 pre-pass: {len(targets)} champions (force={force}, erode={ERODE_PX}px)")
    session = new_session("u2net")
    for slug in targets:
        try:
            log("  " + matte_one(session, slug, force))
        except Exception as e:  # noqa: BLE001
            log(f"  {slug}: ERROR {type(e).__name__}: {str(e)[:200]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
