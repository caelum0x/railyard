"""A/B image-to-3D generator bake-off for Aeonrift champions (FULLY HEADLESS).

For each champion, from the SAME reference.png, generate a GLB with every available
generator, render each to a consistent headless Blender EEVEE preview, and record
triangle counts + cost. Results are written to results.json for build_ab.py.

Generators:
  - trellis   : baseline firtoz/trellis (reuse existing model.glb; no new spend)
  - hunyuan21 : ndreca/hunyuan3d-2.1   (PBR textures + auto-rig)   ~$0.27/run
  - hunyuan2mv: tencent/hunyuan3d-2mv  (multi-view, single front)  ~$0.11/run
  - (fal Tripo v3 / Meshy v6 only if a FAL key exists — skipped when absent)

The Replicate token is loaded at runtime from videogen/.env.local and NEVER printed.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE / "out"
RENDER_SCRIPT = HERE / "ab_render.py"

ENV_FILE = Path("/Users/arhansubasi/products/videogen/apps/web/.env.local")
REF_BASE = Path("/Users/arhansubasi/products/rail-3d/aeonrift/assets/champions")

# display slug -> asset dir slug (reference.png + baseline model.glb live here)
CHAMPS = {
    "anubis": "anubis-warden-of-scales",
    "zeus": "zeus",
}

# Per-run cost estimates (USD). Baseline reuse = no new spend this run.
COST = {
    "trellis": 0.00,      # reused existing model (prior run)
    "hunyuan21": 0.27,
    "hunyuan2mv": 0.11,
    "tripo3d": 0.30,      # fal (only if key)
    "meshy6": 0.30,       # fal (only if key)
}

GEN_LABEL = {
    "trellis": "TRELLIS (firtoz/trellis)",
    "hunyuan21": "Hunyuan3D-2.1 (ndreca/hunyuan3d-2.1)",
    "hunyuan2mv": "Hunyuan3D-2mv (tencent/hunyuan3d-2mv)",
    "tripo3d": "Tripo v3 (fal-ai/tripo3d)",
    "meshy6": "Meshy v6 (fal-ai/meshy/v6)",
}


def load_replicate_token() -> str:
    tok = os.environ.get("REPLICATE_API_TOKEN")
    if tok and tok.startswith("r8_"):
        return tok
    for line in ENV_FILE.read_text().splitlines():
        m = re.match(r"\s*(?:export\s+)?REPLICATE_API_TOKEN\s*=\s*(.+)\s*$", line)
        if m:
            tok = m.group(1).strip().strip('"').strip("'")
            break
    if not tok or not tok.startswith("r8_"):
        raise SystemExit("STOP: REPLICATE_API_TOKEN missing/invalid")
    os.environ["REPLICATE_API_TOKEN"] = tok
    return tok


def find_fal_key() -> str | None:
    for name in ("FAL_KEY", "FAL_API_KEY", "FAL_KEY_ID"):
        v = os.environ.get(name)
        if v:
            return v
    if ENV_FILE.is_file():
        for line in ENV_FILE.read_text().splitlines():
            m = re.match(r"\s*(?:export\s+)?(FAL_KEY|FAL_API_KEY|FAL_KEY_ID)\s*=\s*(.+)\s*$", line)
            if m and m.group(2).strip().strip('"').strip("'"):
                return m.group(2).strip().strip('"').strip("'")
    return None


def is_valid_glb(path: Path) -> bool:
    try:
        with path.open("rb") as f:
            return f.read(4) == b"glTF"
    except Exception:
        return False


def extract_glb_url(output) -> str | None:
    """Find a GLB url in a Replicate prediction output (dict/str/list/FileOutput)."""
    def as_url(x):
        if isinstance(x, str):
            return x
        if hasattr(x, "url"):
            return x.url
        return None

    if isinstance(output, dict):
        # prefer keys that look like a model file
        for key in ("mesh", "model_file", "glb", "model", "output", "file"):
            if key in output and output[key]:
                u = as_url(output[key])
                if u:
                    return u
        for v in output.values():
            u = as_url(v)
            if u and ".glb" in u.lower():
                return u
        for v in output.values():
            u = as_url(v)
            if u:
                return u
        return None
    if isinstance(output, (list, tuple)):
        for v in output:
            u = as_url(v)
            if u and ".glb" in u.lower():
                return u
        return as_url(output[0]) if output else None
    return as_url(output)


def with_retry(fn, what, max_tries=5):
    delay = 15.0
    for attempt in range(1, max_tries + 1):
        try:
            return fn()
        except Exception as e:  # noqa: BLE001
            msg = str(e)
            transient = any(t in msg.lower() for t in ("429", "throttl", "rate limit", "502", "503"))
            if not transient or attempt == max_tries:
                raise
            print(f"    [{what}] transient ({msg[:60]}...) retry {attempt}/{max_tries} in {delay:.0f}s", flush=True)
            time.sleep(delay)
            delay = min(delay * 1.6, 90.0)


def run_replicate(client, model_name, ref: Path, input_overrides: dict, image_field: str) -> tuple[str, float]:
    """Create a prediction, wait, return (glb_url, predict_time_seconds)."""
    model = client.models.get(model_name)
    version = model.versions.list()[0]

    def _create():
        with ref.open("rb") as fh:
            data = dict(input_overrides)
            data[image_field] = fh
            return client.predictions.create(version=version.id, input=data)

    pred = with_retry(_create, model_name)
    pred.wait()
    if pred.status != "succeeded":
        raise RuntimeError(f"{model_name} status={pred.status} err={str(pred.error)[:160]}")
    url = extract_glb_url(pred.output)
    if not url:
        raise RuntimeError(f"{model_name} no glb url: {type(pred.output)} {str(pred.output)[:160]}")
    ptime = 0.0
    try:
        ptime = float((pred.metrics or {}).get("predict_time") or 0.0)
    except Exception:
        pass
    return url, ptime


def download(url: str, out: Path) -> None:
    tmp = out.with_suffix(".tmp")
    urllib.request.urlretrieve(url, tmp)
    if not is_valid_glb(tmp):
        tmp.unlink(missing_ok=True)
        raise RuntimeError("downloaded file is not a valid GLB")
    tmp.replace(out)


def render(glb: Path, png: Path) -> int:
    """Render GLB -> PNG headless; return triangle count (0 on failure)."""
    blender = shutil.which("blender") or "/Applications/Blender.app/Contents/MacOS/Blender"
    cmd = [blender, "--background", "--factory-startup", "--python", str(RENDER_SCRIPT),
           "--", str(glb), str(png)]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=400)
    if not (png.exists() and png.stat().st_size > 1000):
        (png.parent / f"{png.stem}.render_error.log").write_text(
            (r.stdout or "")[-4000:] + "\n---STDERR---\n" + (r.stderr or "")[-4000:])
        return 0
    meta = Path(str(png) + ".meta.json")
    if meta.exists():
        try:
            return int(json.loads(meta.read_text()).get("tris", 0))
        except Exception:
            return 0
    return 0


def main() -> int:
    tok = load_replicate_token()
    import replicate
    client = replicate.Client(api_token=tok)

    fal_key = find_fal_key()
    fal_available = bool(fal_key)
    print(f"fal key present: {fal_available}", flush=True)

    results = {"champions": {}, "fal_available": fal_available, "gen_label": GEN_LABEL, "cost": COST}

    for disp, slug in CHAMPS.items():
        ref = REF_BASE / slug / "reference.png"
        baseline = REF_BASE / slug / "model.glb"
        cdir = OUT / disp
        cdir.mkdir(parents=True, exist_ok=True)
        # copy the reference for the HTML (relative asset)
        shutil.copy2(ref, cdir / "reference.png")
        champ = {"slug": slug, "ref": "reference.png", "generators": {}}
        print(f"\n=== {disp} ({slug}) ===", flush=True)

        # ---- baseline TRELLIS: reuse existing model ----
        try:
            tg = cdir / "trellis.glb"
            shutil.copy2(baseline, tg)
            tp = cdir / "trellis.png"
            tris = render(tg, tp)
            champ["generators"]["trellis"] = {
                "ok": True, "glb": "trellis.glb", "png": "trellis.png",
                "tris": tris, "bytes": tg.stat().st_size,
                "cost": COST["trellis"], "predict_time": None, "note": "reused existing baseline",
            }
            print(f"  trellis: reused baseline, tris={tris}", flush=True)
        except Exception as e:  # noqa: BLE001
            champ["generators"]["trellis"] = {"ok": False, "error": f"{type(e).__name__}: {str(e)[:200]}"}
            print(f"  trellis FAILED: {e}", flush=True)

        # ---- ndreca/hunyuan3d-2.1 ----
        try:
            url, ptime = run_replicate(
                client, "ndreca/hunyuan3d-2.1", ref,
                {"steps": 50, "guidance_scale": 7.5, "max_facenum": 40000,
                 "octree_resolution": 256, "remove_background": True, "generate_texture": True},
                image_field="image",
            )
            hg = cdir / "hunyuan21.glb"
            download(url, hg)
            hp = cdir / "hunyuan21.png"
            tris = render(hg, hp)
            champ["generators"]["hunyuan21"] = {
                "ok": True, "glb": "hunyuan21.glb", "png": "hunyuan21.png",
                "tris": tris, "bytes": hg.stat().st_size,
                "cost": COST["hunyuan21"], "predict_time": round(ptime, 1),
            }
            print(f"  hunyuan21: ok tris={tris} predict_time={ptime:.1f}s", flush=True)
        except Exception as e:  # noqa: BLE001
            champ["generators"]["hunyuan21"] = {"ok": False, "error": f"{type(e).__name__}: {str(e)[:200]}"}
            print(f"  hunyuan21 FAILED: {e}", flush=True)

        # ---- tencent/hunyuan3d-2mv (single front view) ----
        try:
            url, ptime = run_replicate(
                client, "tencent/hunyuan3d-2mv", ref,
                {"steps": 30, "guidance_scale": 5.0, "octree_resolution": 256,
                 "remove_background": True, "target_face_num": 40000, "file_type": "glb"},
                image_field="front_image",
            )
            mg = cdir / "hunyuan2mv.glb"
            download(url, mg)
            mp = cdir / "hunyuan2mv.png"
            tris = render(mg, mp)
            champ["generators"]["hunyuan2mv"] = {
                "ok": True, "glb": "hunyuan2mv.glb", "png": "hunyuan2mv.png",
                "tris": tris, "bytes": mg.stat().st_size,
                "cost": COST["hunyuan2mv"], "predict_time": round(ptime, 1),
            }
            print(f"  hunyuan2mv: ok tris={tris} predict_time={ptime:.1f}s", flush=True)
        except Exception as e:  # noqa: BLE001
            champ["generators"]["hunyuan2mv"] = {"ok": False, "error": f"{type(e).__name__}: {str(e)[:200]}"}
            print(f"  hunyuan2mv FAILED: {e}", flush=True)

        results["champions"][disp] = champ

    (HERE / "results.json").write_text(json.dumps(results, indent=2))
    print("\nWROTE results.json", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
