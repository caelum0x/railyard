#!/usr/bin/env python3
"""League-splash-STYLE art exploration pass (STYLE SELECTION, not final batch).

Generates a FEW champions in 2-3 distinct high-quality approaches so the owner can
pick the aesthetic before we batch all 47. Goal: match the LOOK of League of Legends
splash art -- dynamic action pose, cinematic moody lighting, atmospheric environment,
painterly semi-realistic -- via PROMPTING + generic high-quality fantasy-splash style.
No LoRA, no copyrighted training data. Original-IP-clean.

FULLY HEADLESS: writes PNG files only, never opens a window.

Token: read at runtime from videogen/.env.local (REPLICATE_API_TOKEN). NEVER printed.

Approaches per champion:
  A = black-forest-labs/flux-1.1-pro  (strong splash PROMPT, 4:5)
  B = recraft-ai/recraft-v3           (painterly digital-illustration model, 4:5)
  C = stability-ai/sdxl               (SDXL painterly WITH hard negatives, low-angle heroic comp)
Then an UPSCALE/detail pass (philz1337x/clarity-upscaler) on the flux (A) variant
of each -> <slug>__A_flux__hires.png to show the polish gain.

Resumable: skips any output PNG that already exists. Honors 429 retry_after.
"""
from __future__ import annotations
import json, os, sys, time, urllib.request, urllib.error
from pathlib import Path

HERE = Path(__file__).resolve().parent
AEON = HERE.parent.parent
OUT = AEON / "assets" / "cards" / "style-test"
OUT.mkdir(parents=True, exist_ok=True)
MANIFEST = OUT / "manifest.json"
ENV_LOCAL = Path("/Users/arhansubasi/products/videogen/apps/web/.env.local")
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"

# ---- style knobs (baked with owner's atmospheric-cinematic reference guidance) ----
STYLE_SUFFIX = (
    "League of Legends splash art style, dynamic full-body action pose mid-motion, "
    "low dramatic camera angle, cinematic key-art composition, moody dark atmospheric "
    "environment, strong rim lighting with a glowing magical accent color, volumetric fog "
    "and floating particles, painterly semi-realistic digital illustration, ornate highly "
    "detailed fantasy armor and costume, rich cinematic color grade, sense of depth and "
    "atmosphere, tells a small story, epic fantasy concept art, artstation trending, "
    "highly detailed, sharp focus"
)
NEGATIVE = (
    "static centered portrait, standing still, flat lighting, plain empty background, "
    "anime, cel shaded, manga, cartoon, photograph, photorealistic photo, 3d render, "
    "blurry, out of focus, deformed hands, extra fingers, extra limbs, mutated, watermark, "
    "signature, text, caption, logo, ui, card frame, border, low detail, amateur, jpeg artifacts"
)

# per-champion subject: dynamic pose + environment + magical secondary element (a little story)
SUBJECTS = {
    "zeus": (
        "Zeus king of Olympus, a mighty bearded Greek storm god, lunging from a low heroic "
        "angle as he hurls a crackling forked lightning bolt, arcs of blue-white electricity "
        "coiling up his arm, roiling thunderclouds and glowing storm energy swirling behind, "
        "shattered marble columns of Olympus, ornate golden greek armor and a wind-torn cape"
    ),
    "anubis-warden-of-scales": (
        "Anubis warden of scales, a towering jackal-headed Egyptian death god, kneeling low as "
        "he slams golden scales of judgment into the ground, ghostly turquoise spirits and "
        "glowing hieroglyphs streaming upward around him, soul-light and drifting sand, ornate "
        "gold-and-obsidian ceremonial armor, dark torchlit tomb"
    ),
    "mars": (
        "Mars disciplined Roman god of war, lunging spear-first mid-dash toward the viewer, a "
        "trail of crimson battle-energy streaking off the spearhead, legion banners and drifting "
        "embers behind, dark storm-lit battlefield, ornate crimson-and-bronze lorica armor and "
        "a plumed centurion helm"
    ),
    "nyxralla": (
        "Nyxralla abyssal tide sorceress, arms sweeping wide from a low angle as she conjures a "
        "spiraling maelstrom of dark water and glowing cyan abyssal energy, ribbons of water and "
        "bioluminescent light coiling around her, sunken ruins in a deep ocean trench, ornate "
        "teal-and-pearl armor and flowing translucent robes"
    ),
    "vashka": (
        "Vashka the Cindermaw, a feral ember bruiser, charging mid-leap through a burst of fire, "
        "molten cracks and burning cinders trailing from her armor, glowing orange embers and "
        "black smoke swirling, dark volcanic ashland at night, ornate scorched-iron armor veined "
        "with glowing lava"
    ),
}
# C uses an alternate composition to test dynamism
COMP_C = (
    "extreme low-angle heroic shot looking up, over-the-shoulder dynamic framing, "
    "the hero towering and lunging into frame"
)

APPROACHES = [
    {"key": "A_flux",    "model": "black-forest-labs/flux-1.1-pro"},
    {"key": "B_recraft", "model": "recraft-ai/recraft-v3"},
    {"key": "C_sdxl",    "model": "stability-ai/sdxl"},
]

# --------------------------------------------------------------------------- io
def load_token() -> str:
    for line in ENV_LOCAL.read_text().splitlines():
        if line.startswith("REPLICATE_API_TOKEN"):
            t = line.split("=", 1)[1].strip().strip('"').strip("'")
            if t.startswith("r8_"):
                return t
    raise SystemExit("REPLICATE_API_TOKEN not found in env.local")

TOKEN = load_token()

def _headers(extra=None):
    h = {"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json", "User-Agent": UA}
    if extra:
        h.update(extra)
    return h

_last_create = [0.0]
MIN_GAP = 11.0  # ~6/min with headroom

def create_prediction(model: str, inp: dict) -> dict:
    """POST to the model predictions endpoint with Prefer: wait; retry on 429."""
    url = f"https://api.replicate.com/v1/models/{model}/predictions"
    body = json.dumps({"input": inp}).encode()
    while True:
        gap = MIN_GAP - (time.time() - _last_create[0])
        if gap > 0:
            time.sleep(gap)
        req = urllib.request.Request(url, data=body, headers=_headers({"Prefer": "wait"}), method="POST")
        try:
            with urllib.request.urlopen(req, timeout=180) as r:
                _last_create[0] = time.time()
                return json.load(r)
        except urllib.error.HTTPError as e:
            _last_create[0] = time.time()
            if e.code == 429:
                try:
                    j = json.loads(e.read().decode())
                    wait = float(j.get("retry_after", 6)) + 1
                except Exception:
                    wait = 8
                print(f"    429 rate-limited; waiting {wait:.0f}s", flush=True)
                time.sleep(wait)
                continue
            raise RuntimeError(f"HTTP {e.code}: {e.read().decode()[:300]}")

def poll(pred: dict) -> dict:
    """If not terminal, poll the get URL until done."""
    if pred.get("status") in ("succeeded", "failed", "canceled"):
        return pred
    get_url = (pred.get("urls") or {}).get("get")
    if not get_url:
        return pred
    for _ in range(120):
        time.sleep(3)
        req = urllib.request.Request(get_url, headers=_headers(), method="GET")
        with urllib.request.urlopen(req, timeout=60) as r:
            pred = json.load(r)
        if pred.get("status") in ("succeeded", "failed", "canceled"):
            return pred
    return pred

def first_url(output) -> str | None:
    if isinstance(output, str):
        return output
    if isinstance(output, list) and output:
        return output[0] if isinstance(output[0], str) else None
    return None

def download(url: str, dest: Path):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=180) as r:
        dest.write_bytes(r.read())

# ----------------------------------------------------------------- input shaping
def build_input(approach_key: str, slug: str, subject: str) -> dict:
    prompt = f"{subject}. {STYLE_SUFFIX}"
    if approach_key == "A_flux":
        return {"prompt": prompt, "aspect_ratio": "4:5", "output_format": "png",
                "prompt_upsampling": True, "safety_tolerance": 5}
    if approach_key == "B_recraft":
        return {"prompt": prompt, "size": "1024x1365", "style": "digital_illustration"}
    if approach_key == "C_sdxl":
        cprompt = f"{subject}. {COMP_C}. {STYLE_SUFFIX}"
        return {"prompt": cprompt, "negative_prompt": NEGATIVE, "width": 1024, "height": 1280,
                "num_inference_steps": 40, "guidance_scale": 7.5, "scheduler": "KarrasDPM",
                "refine": "expert_ensemble_refiner", "apply_watermark": False}
    raise ValueError(approach_key)

# --------------------------------------------------------------------------- run
def load_manifest() -> dict:
    if MANIFEST.exists():
        return json.loads(MANIFEST.read_text())
    return {}

def save_manifest(m: dict):
    MANIFEST.write_text(json.dumps(m, indent=2))

def main():
    only = None
    if "--only" in sys.argv:
        only = sys.argv[sys.argv.index("--only") + 1]
    slugs = [only] if only else list(SUBJECTS.keys())
    manifest = load_manifest()

    for slug in slugs:
        subject = SUBJECTS[slug]
        manifest.setdefault(slug, {})
        for ap in APPROACHES:
            key = ap["key"]
            dest = OUT / f"{slug}__{key}.png"
            rec = manifest[slug].setdefault(key, {})
            rec["model"] = ap["model"]
            rec["prompt"] = build_input(key, slug, subject).get("prompt")
            if key == "C_sdxl":
                rec["negative"] = NEGATIVE
            if dest.exists() and dest.stat().st_size > 1000:
                print(f"[skip] {dest.name} exists", flush=True)
                continue
            print(f"[gen ] {slug} :: {key} ({ap['model']})", flush=True)
            try:
                pred = poll(create_prediction(ap["model"], build_input(key, slug, subject)))
            except Exception as e:
                print(f"    ERROR create: {e}", flush=True)
                rec["error"] = str(e)[:300]
                save_manifest(manifest)
                continue
            if pred.get("status") != "succeeded":
                err = pred.get("error") or pred.get("status")
                print(f"    FAILED: {err}", flush=True)
                rec["error"] = str(err)[:300]
                save_manifest(manifest)
                continue
            url = first_url(pred.get("output"))
            if not url:
                print("    no output url", flush=True)
                rec["error"] = "no output url"
                save_manifest(manifest)
                continue
            download(url, dest)
            rec["file"] = dest.name
            rec["output_url"] = url
            rec.pop("error", None)
            print(f"    saved {dest.name} ({dest.stat().st_size//1024} KB)", flush=True)
            save_manifest(manifest)

        # ---- upscale/detail pass on the flux (A) variant to show polish gain ----
        a = manifest[slug].get("A_flux", {})
        hires = OUT / f"{slug}__A_flux__hires.png"
        if a.get("output_url") and not (hires.exists() and hires.stat().st_size > 1000):
            print(f"[hi-r] {slug} :: upscaling A_flux via clarity-upscaler", flush=True)
            try:
                up = poll(create_prediction("philz1337x/clarity-upscaler", {
                    "image": a["output_url"], "scale_factor": 2,
                    "creativity": 0.35, "resemblance": 0.6, "dynamic": 6,
                }))
                if up.get("status") == "succeeded":
                    uurl = first_url(up.get("output"))
                    if uurl:
                        download(uurl, hires)
                        manifest[slug]["A_flux_hires"] = {
                            "model": "philz1337x/clarity-upscaler",
                            "file": hires.name, "output_url": uurl,
                            "base": a.get("file"),
                        }
                        print(f"    saved {hires.name} ({hires.stat().st_size//1024} KB)", flush=True)
                    else:
                        print("    upscale: no output url", flush=True)
                else:
                    print(f"    upscale FAILED: {up.get('error') or up.get('status')}", flush=True)
            except Exception as e:
                print(f"    upscale ERROR: {e}", flush=True)
            save_manifest(manifest)

    save_manifest(manifest)
    print("DONE", flush=True)

if __name__ == "__main__":
    main()
