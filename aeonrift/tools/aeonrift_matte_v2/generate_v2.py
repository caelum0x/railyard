"""Matte-v2 Hunyuan3D-2.1 regeneration for the flagged base-artifact champions (HEADLESS).

Regenerates each flagged champion from its TIGHTER matte-v2 reference (see matte_v2.py),
validates the GLB as glTF v2 (same assertion as the Rust asset test), enforces the 40k-tri
cap, and renders a headless Blender EEVEE preview with the SAME rig as the gallery.

Two background variants are supported so the reconstruction with the LEAST base geometry
can be chosen empirically:
  * transparent -> reference.matte2.png       (subject on transparent alpha)
  * gray        -> reference.matte2_gray.png  (subject on solid #808080)

Outputs (per job) land in out/<slug>/:
  model.<variant>.glb , preview.<variant>.png

The Replicate token is loaded at runtime from videogen/.env.local and NEVER printed.
Idempotent/resumable; paced ~11s; honors 429/retry-after; STOPS gracefully on credit-out.

Usage:
  # A/B probe: both backgrounds for two champions
  .venv/bin/python generate_v2.py --jobs cyranth:transparent cyranth:gray xelphine:transparent xelphine:gray
  # Bulk: one background for the rest
  .venv/bin/python generate_v2.py --variant gray kaeza korrathil mars veyl zeryth nyxralla petalsong artemis vulcan
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
RESULTS = OUT / "results_v2.json"
RENDER_SCRIPT = HERE / "render_preview.py"

ENV_FILE = Path("/Users/arhansubasi/products/videogen/apps/web/.env.local")
SRC_BASE = Path("/Users/arhansubasi/products/rail-matte/aeonrift/assets/champions")

MODEL_NAME = "ndreca/hunyuan3d-2.1"
COST_PER_RUN = 0.27
PACE_SECONDS = 11.0
MAX_TRIS = 40000

VARIANT_REF = {
    "transparent": "reference.matte2.png",
    "gray": "reference.matte2_gray.png",
}

HUNYUAN_INPUT = {
    "steps": 50,
    "guidance_scale": 7.5,
    "max_facenum": MAX_TRIS,
    "octree_resolution": 256,
    "remove_background": True,
    "generate_texture": True,
}

BALANCE_MARKERS = (
    "insufficient", "spend limit", "billing", "payment required", "402",
    "out of credit", "quota exceeded", "balance", "purchase credit",
)


class StopBalance(RuntimeError):
    """Raised when Replicate signals the account is out of credit."""


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def load_replicate_token() -> str:
    tok = os.environ.get("REPLICATE_API_TOKEN")
    if not (tok and tok.startswith("r8_")):
        tok = None
        for line in ENV_FILE.read_text().splitlines():
            m = re.match(r"\s*(?:export\s+)?REPLICATE_API_TOKEN\s*=\s*(.+)\s*$", line)
            if m:
                tok = m.group(1).strip().strip('"').strip("'")
                break
    if not tok or not tok.startswith("r8_"):
        raise SystemExit("STOP: REPLICATE_API_TOKEN missing/invalid")
    os.environ["REPLICATE_API_TOKEN"] = tok
    return tok


def assert_valid_glb(path: Path) -> None:
    data = path.read_bytes()
    if len(data) < 12:
        raise ValueError(f"glb shorter than 12-byte header ({len(data)} bytes)")
    if data[0:4] != b"glTF":
        raise ValueError("bad glb magic (expected 'glTF')")
    if int.from_bytes(data[4:8], "little") != 2:
        raise ValueError("unsupported glTF version")
    if int.from_bytes(data[8:12], "little") != len(data):
        raise ValueError("glb declared length != actual")
    if data[16:20] != b"JSON":
        raise ValueError("first chunk is not JSON")


def extract_glb_url(output) -> str | None:
    def as_url(x):
        if isinstance(x, str):
            return x
        if hasattr(x, "url"):
            return x.url
        return None

    if isinstance(output, dict):
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


def is_balance_error(msg: str) -> bool:
    low = msg.lower()
    return any(m in low for m in BALANCE_MARKERS)


def parse_retry_after(msg: str) -> float | None:
    m = re.search(r"retry[-_ ]after[\"']?\s*[:=]?\s*(\d+(?:\.\d+)?)", msg, re.I)
    return float(m.group(1)) if m else None


def create_with_backoff(client, version_id: str, ref: Path, max_tries: int = 6):
    delay = 15.0
    for attempt in range(1, max_tries + 1):
        try:
            with ref.open("rb") as fh:
                data = dict(HUNYUAN_INPUT)
                data["image"] = fh
                return client.predictions.create(version=version_id, input=data)
        except Exception as e:  # noqa: BLE001
            msg = str(e)
            if is_balance_error(msg):
                raise StopBalance(msg) from e
            transient = any(t in msg.lower()
                            for t in ("429", "throttl", "rate limit", "502", "503", "timeout"))
            if not transient or attempt == max_tries:
                raise
            wait = parse_retry_after(msg) or delay
            log(f"    transient create error ({msg[:70]}...) retry {attempt}/{max_tries} in {wait:.0f}s")
            time.sleep(wait)
            delay = min(delay * 1.7, 90.0)


def run_hunyuan(client, version_id: str, ref: Path) -> tuple[str, float]:
    pred = create_with_backoff(client, version_id, ref)
    pred.wait()
    if pred.status != "succeeded":
        err = str(pred.error or "")
        if is_balance_error(err):
            raise StopBalance(err)
        raise RuntimeError(f"status={pred.status} err={err[:200]}")
    url = extract_glb_url(pred.output)
    if not url:
        raise RuntimeError(f"no glb url in output: {type(pred.output)} {str(pred.output)[:160]}")
    ptime = 0.0
    try:
        ptime = float((pred.metrics or {}).get("predict_time") or 0.0)
    except Exception:
        pass
    return url, ptime


def download(url: str, out: Path) -> None:
    tmp = out.with_suffix(".tmp")
    urllib.request.urlretrieve(url, tmp)
    try:
        assert_valid_glb(tmp)
    except Exception:
        tmp.unlink(missing_ok=True)
        raise
    tmp.replace(out)


def render(glb: Path, png: Path) -> int:
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


def load_results() -> dict:
    if RESULTS.is_file():
        try:
            return json.loads(RESULTS.read_text())
        except Exception:
            pass
    return {"model": MODEL_NAME, "version_id": None, "cost_per_run": COST_PER_RUN,
            "spend": 0.0, "jobs": {}}


def save_results(results: dict) -> None:
    RESULTS.write_text(json.dumps(results, indent=2))


def parse_jobs(argv: list[str]) -> list[tuple[str, str]]:
    """--jobs slug:variant ...   OR   --variant V slug slug ..."""
    jobs: list[tuple[str, str]] = []
    if "--jobs" in argv:
        i = argv.index("--jobs")
        for tok in argv[i + 1:]:
            if tok.startswith("-"):
                break
            slug, _, variant = tok.partition(":")
            variant = variant or "gray"
            if variant not in VARIANT_REF:
                raise SystemExit(f"bad variant {variant!r} in job {tok!r}")
            jobs.append((slug, variant))
        return jobs
    variant = "gray"
    if "--variant" in argv:
        variant = argv[argv.index("--variant") + 1]
        if variant not in VARIANT_REF:
            raise SystemExit(f"bad --variant {variant!r}")
    slugs = [a for a in argv if not a.startswith("-") and a not in VARIANT_REF]
    return [(s, variant) for s in slugs]


def main() -> int:
    tok = load_replicate_token()
    import replicate
    client = replicate.Client(api_token=tok)

    model = client.models.get(MODEL_NAME)
    version_id = model.versions.list()[0].id
    model_source = f"{MODEL_NAME}:{version_id}"
    log(f"resolved {model_source}")

    jobs = parse_jobs(sys.argv[1:])
    if not jobs:
        raise SystemExit("no jobs. use --jobs slug:variant ... or --variant V slug ...")
    log(f"{len(jobs)} generation jobs")

    results = load_results()
    results["model"] = MODEL_NAME
    results["version_id"] = version_id
    results["model_source"] = model_source

    created = 0
    for i, (slug, variant) in enumerate(jobs, 1):
        cdir = OUT / slug
        cdir.mkdir(parents=True, exist_ok=True)
        glb = cdir / f"model.{variant}.glb"
        png = cdir / f"preview.{variant}.png"
        key = f"{slug}:{variant}"
        rec = results["jobs"].get(key, {})

        if glb.is_file():
            try:
                assert_valid_glb(glb)
                if not png.is_file():
                    rec["tris"] = render(glb, png)
                if rec.get("ok"):
                    log(f"[{i}/{len(jobs)}] {key}: already done (skip)")
                    results["jobs"][key] = rec
                    save_results(results)
                    continue
            except Exception:
                glb.unlink(missing_ok=True)

        ref = SRC_BASE / slug / VARIANT_REF[variant]
        if not ref.is_file():
            log(f"[{i}/{len(jobs)}] {key}: MISSING ref {ref.name} — skip")
            results["jobs"][key] = {"ok": False, "error": f"missing ref {ref.name}"}
            save_results(results)
            continue

        log(f"[{i}/{len(jobs)}] {key}: generating... (ref={ref.name})")
        try:
            url, ptime = run_hunyuan(client, version_id, ref)
            download(url, glb)
            tris = render(glb, png)
            rec = {
                "ok": True, "slug": slug, "variant": variant,
                "glb": str(glb), "png": str(png), "tris": tris,
                "tris_ok": tris <= int(MAX_TRIS * 1.02),
                "bytes": glb.stat().st_size, "predict_time": round(ptime, 1),
                "cost": COST_PER_RUN, "model_source": model_source,
            }
            results["spend"] = round(results.get("spend", 0.0) + COST_PER_RUN, 2)
            created += 1
            log(f"    ok tris={tris} bytes={rec['bytes']} predict_time={ptime:.1f}s "
                f"spend=${results['spend']:.2f}")
        except StopBalance as e:  # noqa: BLE001
            log(f"    STOP (balance): {str(e)[:160]}")
            results["jobs"][key] = {"ok": False, "error": f"balance: {str(e)[:200]}"}
            results["stopped_balance"] = True
            save_results(results)
            log(f"STOPPED at {i}/{len(jobs)} due to balance. spend~${results['spend']:.2f}")
            return 2
        except Exception as e:  # noqa: BLE001
            log(f"    FAILED: {type(e).__name__}: {str(e)[:160]}")
            rec = {"ok": False, "slug": slug, "variant": variant,
                   "error": f"{type(e).__name__}: {str(e)[:200]}"}

        results["jobs"][key] = rec
        save_results(results)

        if rec.get("ok") and i < len(jobs):
            time.sleep(PACE_SECONDS)

    ok = sum(1 for r in results["jobs"].values() if r.get("ok"))
    log(f"DONE. ok={ok}/{len(results['jobs'])} created={created} spend~${results.get('spend', 0):.2f}")
    save_results(results)
    return 0


if __name__ == "__main__":
    sys.exit(main())
