"""Generate real AI illustrations for every Aeonrift champion via Replicate Flux.

Fully headless: writes PNG files only, never opens a window. Idempotent -- skips
slugs whose art already exists. Token is read from the environment
(REPLICATE_API_TOKEN); it is never printed or written to disk.

Usage:
    export REPLICATE_API_TOKEN=...        # loaded by the caller, never echoed
    python generate_art.py [--only slug1,slug2] [--model flux-dev|flux-schnell]
"""
from __future__ import annotations

import argparse
import os
import re
import sys
import time
import urllib.request
from pathlib import Path

import replicate

from champions import load_all, Champion

ART_DIR = Path(__file__).resolve().parent / "art"
MODELS = {
    "flux-dev": "black-forest-labs/flux-dev",
    "flux-schnell": "black-forest-labs/flux-schnell",
}


def _write_output(out, dest: Path) -> None:
    """Replicate output may be a FileOutput, list, or URL string."""
    item = out[0] if isinstance(out, (list, tuple)) else out
    if hasattr(item, "read"):
        data = item.read()
        dest.write_bytes(data)
        return
    url = str(item)
    with urllib.request.urlopen(url, timeout=120) as resp:
        dest.write_bytes(resp.read())


def _run_one(champ: Champion, model_key: str) -> None:
    model = MODELS[model_key]
    inp = {
        "prompt": champ.prompt(),
        "aspect_ratio": "2:3",       # ~832x1216 portrait
        "num_outputs": 1,
        "output_format": "png",
        "output_quality": 95,
        "num_inference_steps": 28 if model_key == "flux-dev" else 4,
    }
    if model_key == "flux-dev":
        inp["guidance"] = 3.2
    out = replicate.run(model, input=inp)
    dest = ART_DIR / f"{champ.slug}.png"
    _write_output(out, dest)
    if not dest.exists() or dest.stat().st_size < 1000:
        raise RuntimeError("empty/tiny output file")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", default="")
    ap.add_argument("--model", default="flux-dev", choices=list(MODELS))
    args = ap.parse_args()

    if not os.environ.get("REPLICATE_API_TOKEN"):
        print("ERROR: REPLICATE_API_TOKEN not set in environment", file=sys.stderr)
        return 2

    ART_DIR.mkdir(parents=True, exist_ok=True)
    champs = load_all()
    if args.only:
        want = {s.strip() for s in args.only.split(",")}
        champs = [c for c in champs if c.slug in want]

    ok, skipped, failed = [], [], []
    for i, c in enumerate(champs, 1):
        dest = ART_DIR / f"{c.slug}.png"
        if dest.exists() and dest.stat().st_size > 1000:
            skipped.append(c.slug)
            print(f"[{i}/{len(champs)}] skip (exists): {c.slug}")
            continue
        model_key = args.model
        last_err = None
        attempt = 0
        real_errors = 0          # non-throttle failures (these can trigger fallback)
        while attempt < 30:
            attempt += 1
            try:
                print(f"[{i}/{len(champs)}] gen {c.slug} ({model_key}) attempt {attempt}")
                _run_one(c, model_key)
                ok.append(c.slug)
                last_err = None
                break
            except Exception as e:  # noqa: BLE001 -- batch resilience
                last_err = e
                msg = str(e).lower()
                print(f"    !! {type(e).__name__}: {e}", file=sys.stderr)
                throttled = "429" in msg or "throttl" in msg or "rate limit" in msg
                if throttled:
                    # Parse "resets in ~Ns"; wait it out. Throttling never
                    # triggers a model fallback -- style must stay cohesive.
                    m = re.search(r"resets in ~?(\d+)", msg)
                    wait = int(m.group(1)) + 3 if m else 12
                    print(f"    -> throttled, sleeping {wait}s")
                    time.sleep(wait)
                    continue
                real_errors += 1
                # Only fall back to schnell after two genuine model errors.
                if model_key == "flux-dev" and real_errors >= 2:
                    model_key = "flux-schnell"
                    print("    -> falling back to flux-schnell")
                if real_errors >= 4:
                    break
                time.sleep(min(2 ** real_errors, 8))
        if last_err is not None:
            failed.append((c.slug, str(last_err)))

    print("\n==== SUMMARY ====")
    print(f"generated: {len(ok)}  skipped(existing): {len(skipped)}  failed: {len(failed)}")
    for slug, err in failed:
        print(f"  FAILED {slug}: {err[:160]}")
    return 0 if not failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
