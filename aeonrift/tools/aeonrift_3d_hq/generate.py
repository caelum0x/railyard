"""Regenerate ALL Aeonrift champion 3D models with ndreca/hunyuan3d-2.1 (FULLY HEADLESS).

For each champion, from the existing reference.png, generate a textured GLB with
ndreca/hunyuan3d-2.1, validate it as real glTF v2 (same assertion the Rust asset test
uses), and render a headless Blender EEVEE preview with the SAME camera/lighting rig as
the existing gallery previews (render_preview.py, copied verbatim from the A/B pipeline).

The Replicate token is loaded at runtime from videogen/.env.local and NEVER printed.

Design notes:
  * Idempotent / resumable: a champion whose GLB is already downloaded + valid is skipped.
  * Paced ~11s between prediction creates (account is throttled to ~6 req/min, burst 1).
  * Honors 429 / retry_after with exponential backoff.
  * Tracks spend; on a balance/credit error it STOPS and reports progress rather than
    failing hard.
  * Writes out/results.json incrementally after every champion.
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
RESULTS = OUT / "results.json"
RENDER_SCRIPT = HERE / "render_preview.py"

ENV_FILE = Path("/Users/arhansubasi/products/videogen/apps/web/.env.local")
# reference.png + baseline TRELLIS model.glb live in the rail-3d (aeon/models-ai) worktree.
SRC_BASE = Path("/Users/arhansubasi/products/rail-3d/aeonrift/assets/champions")

MODEL_NAME = "ndreca/hunyuan3d-2.1"
COST_PER_RUN = 0.27
PACE_SECONDS = 11.0
MAX_TRIS = 40000

HUNYUAN_INPUT = {
    "steps": 50,
    "guidance_scale": 7.5,
    "max_facenum": MAX_TRIS,
    "octree_resolution": 256,
    "remove_background": True,
    "generate_texture": True,
}

# Substrings that indicate the account balance / spend limit is exhausted -> STOP.
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


def champion_slugs() -> list[str]:
    slugs = [p.name for p in sorted(SRC_BASE.iterdir())
             if p.is_dir() and (p / "reference.png").is_file()]
    return slugs


def assert_valid_glb(path: Path) -> None:
    """Same structural assertion as the Rust every_spec_has_a_resolvable_valid_glb test."""
    data = path.read_bytes()
    if len(data) < 12:
        raise ValueError(f"glb shorter than 12-byte header ({len(data)} bytes)")
    if data[0:4] != b"glTF":
        raise ValueError("bad glb magic (expected 'glTF')")
    version = int.from_bytes(data[4:8], "little")
    if version != 2:
        raise ValueError(f"unsupported glTF version {version}")
    declared = int.from_bytes(data[8:12], "little")
    if declared != len(data):
        raise ValueError(f"glb declared length {declared} != actual {len(data)}")
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
    if m:
        return float(m.group(1))
    return None


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
            "spend": 0.0, "champions": {}}


def save_results(results: dict) -> None:
    RESULTS.write_text(json.dumps(results, indent=2))


def main() -> int:
    tok = load_replicate_token()
    import replicate
    client = replicate.Client(api_token=tok)

    model = client.models.get(MODEL_NAME)
    version_id = model.versions.list()[0].id
    model_source = f"{MODEL_NAME}:{version_id}"
    log(f"resolved {model_source}")

    slugs = champion_slugs()
    # Optional CLI args restrict to an explicit slug subset (smoke testing).
    argsel = [a for a in sys.argv[1:] if not a.startswith("-")]
    if argsel:
        slugs = [s for s in slugs if s in set(argsel)]
    log(f"{len(slugs)} champions to process")

    results = load_results()
    results["model"] = MODEL_NAME
    results["version_id"] = version_id
    results["model_source"] = model_source

    processed_new = 0
    for i, slug in enumerate(slugs, 1):
        cdir = OUT / slug
        cdir.mkdir(parents=True, exist_ok=True)
        glb = cdir / "model.hunyuan.glb"
        png = cdir / "preview.png"
        rec = results["champions"].get(slug, {})

        # Resume: valid GLB already present -> ensure preview then skip generation.
        if glb.is_file():
            try:
                assert_valid_glb(glb)
                if not png.is_file():
                    tris = render(glb, png)
                    rec["tris"] = tris
                if rec.get("ok"):
                    log(f"[{i}/{len(slugs)}] {slug}: already done (skip)")
                    results["champions"][slug] = rec
                    save_results(results)
                    continue
            except Exception:
                glb.unlink(missing_ok=True)

        # Prefer the background-matted reference (subject on neutral gray) when present:
        # it eliminates Hunyuan's flat white/colored base-geometry artifact. Fall back to
        # the raw concept reference.png for champions with no matte.
        matte = SRC_BASE / slug / "reference.matte.png"
        ref = matte if matte.is_file() else (SRC_BASE / slug / "reference.png")
        log(f"[{i}/{len(slugs)}] {slug}: generating... (ref={ref.name})")
        try:
            url, ptime = run_hunyuan(client, version_id, ref)
            download(url, glb)
            tris = render(glb, png)
            rec = {
                "ok": True,
                "glb": str(glb),
                "png": str(png),
                "tris": tris,
                "tris_ok": tris <= int(MAX_TRIS * 1.02),
                "bytes": glb.stat().st_size,
                "predict_time": round(ptime, 1),
                "cost": COST_PER_RUN,
                "model_source": model_source,
            }
            results["spend"] = round(results.get("spend", 0.0) + COST_PER_RUN, 2)
            processed_new += 1
            log(f"    ok tris={tris} bytes={rec['bytes']} predict_time={ptime:.1f}s "
                f"spend=${results['spend']:.2f}")
        except StopBalance as e:  # noqa: BLE001
            log(f"    STOP (balance): {str(e)[:160]}")
            rec = {"ok": False, "error": f"balance: {str(e)[:200]}"}
            results["champions"][slug] = rec
            results["stopped_balance"] = True
            save_results(results)
            log(f"STOPPED at {i}/{len(slugs)} due to balance. spend~${results['spend']:.2f}")
            return 2
        except Exception as e:  # noqa: BLE001
            log(f"    FAILED: {type(e).__name__}: {str(e)[:160]}")
            rec = {"ok": False, "error": f"{type(e).__name__}: {str(e)[:200]}"}

        results["champions"][slug] = rec
        save_results(results)

        # Pace only when we actually created a prediction and more remain.
        if rec.get("ok") and i < len(slugs):
            time.sleep(PACE_SECONDS)

    ok = sum(1 for r in results["champions"].values() if r.get("ok"))
    log(f"DONE. ok={ok}/{len(slugs)} new={processed_new} spend~${results.get('spend', 0):.2f}")
    save_results(results)
    return 0


if __name__ == "__main__":
    sys.exit(main())
