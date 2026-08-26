#!/usr/bin/env python3
"""AI-illustration pipeline hook for the Ascendancy card art slots.

KEY-GATED and SAFE BY DEFAULT. With NO API key set this script is a clean no-op:
it prints the prompt it *would* send and exits, so the card renderer keeps using
the procedural faction sigil. It NEVER invents a key and NEVER calls a live API
unless you explicitly provide both IMAGE_API and IMAGE_API_KEY.

    # Dry run (default) — prints prompts, writes nothing, calls nothing:
    python3 tools/cardgen/generate_illustrations.py

    # Real run — you supply the provider + key, then re-render cards:
    export IMAGE_API=replicate            # or: openai | stability
    export IMAGE_API_KEY=****             # your key; never hardcoded
    python3 tools/cardgen/generate_illustrations.py --go
    python3 tools/cardgen/render_cards.py       # sigils -> your new art

Output: assets/cards/art/<slug>.png  (1024x1024-ish, the renderer slices it into
the 508x344 art slot with xMidYMid slice).

Providers supported (one is enough): Replicate (Flux), OpenAI Images, Stability.
Cost note: budget ~US$0.003-0.04 per image depending on provider/model, so a full
47-card set is roughly US$0.15-2.00 for one pass. Always dry-run first.
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from ron_parse import load_champion, parse_roster_meta  # noqa: E402
from prompt_style import build_prompt, negative_prompt  # noqa: E402

AEON = HERE.parent.parent
CHAMP_DIR = AEON / "content" / "champions"
ART_DIR = AEON / "assets" / "cards" / "art"


def _slugs() -> list[str]:
    return [p.stem for p in sorted(CHAMP_DIR.glob("*.ron")) if p.stem != "roster_meta"]


# --- provider adapters ------------------------------------------------------
# Each returns raw PNG bytes for a prompt, or raises. They are only ever called
# on an explicit --go run with a real key. Imports are lazy so the dry-run path
# needs zero third-party packages.

def _gen_replicate(prompt: str, key: str) -> bytes:
    import requests  # lazy
    model = os.environ.get("IMAGE_MODEL", "black-forest-labs/flux-schnell")
    r = requests.post(
        "https://api.replicate.com/v1/models/%s/predictions" % model,
        headers={"Authorization": f"Bearer {key}", "Prefer": "wait"},
        json={"input": {"prompt": prompt, "aspect_ratio": "1:1",
                        "output_format": "png", "num_outputs": 1}},
        timeout=120,
    )
    r.raise_for_status()
    out = r.json().get("output")
    url = out[0] if isinstance(out, list) else out
    return requests.get(url, timeout=120).content


def _gen_openai(prompt: str, key: str) -> bytes:
    import base64
    import requests
    model = os.environ.get("IMAGE_MODEL", "gpt-image-1")
    r = requests.post(
        "https://api.openai.com/v1/images/generations",
        headers={"Authorization": f"Bearer {key}"},
        json={"model": model, "prompt": prompt, "size": "1024x1024", "n": 1},
        timeout=180,
    )
    r.raise_for_status()
    return base64.b64decode(r.json()["data"][0]["b64_json"])


def _gen_stability(prompt: str, key: str) -> bytes:
    import requests
    r = requests.post(
        "https://api.stability.ai/v2beta/stable-image/generate/core",
        headers={"Authorization": f"Bearer {key}", "Accept": "image/*"},
        files={"none": ""},
        data={"prompt": prompt, "negative_prompt": negative_prompt(),
              "aspect_ratio": "1:1", "output_format": "png"},
        timeout=180,
    )
    r.raise_for_status()
    return r.content


_PROVIDERS = {
    "replicate": _gen_replicate,
    "openai": _gen_openai,
    "stability": _gen_stability,
}


def main() -> int:
    ap = argparse.ArgumentParser(description="Generate card illustrations (key-gated).")
    ap.add_argument("--go", action="store_true",
                    help="actually call the API (requires IMAGE_API + IMAGE_API_KEY)")
    ap.add_argument("--only", help="single slug")
    ap.add_argument("--force", action="store_true", help="regenerate even if art exists")
    args = ap.parse_args()

    api = os.environ.get("IMAGE_API", "").strip().lower()
    key = os.environ.get("IMAGE_API_KEY", "").strip()

    roster = parse_roster_meta((CHAMP_DIR / "roster_meta.ron").read_text(encoding="utf-8"))
    slugs = _slugs()
    if args.only:
        slugs = [s for s in slugs if s == args.only]

    live = bool(args.go and api and key)
    if not live:
        why = []
        if not args.go:
            why.append("no --go flag")
        if not api:
            why.append("IMAGE_API unset")
        if not key:
            why.append("IMAGE_API_KEY unset")
        print(f"DRY RUN ({', '.join(why)}). Printing prompts; calling nothing; "
              f"writing nothing. Cards keep their procedural sigils.\n")
    else:
        if api not in _PROVIDERS:
            print(f"Unknown IMAGE_API={api!r}; supported: {', '.join(_PROVIDERS)}",
                  file=sys.stderr)
            return 2
        ART_DIR.mkdir(parents=True, exist_ok=True)
        print(f"LIVE via {api}: generating {len(slugs)} illustration(s)...\n")

    gen = _PROVIDERS.get(api)
    made = 0
    for slug in slugs:
        model = load_champion(slug, CHAMP_DIR, roster)
        prompt = build_prompt(model)
        dest = ART_DIR / f"{slug}.png"
        if not live:
            print(f"[{slug}]\n  {prompt}\n")
            continue
        if dest.exists() and not args.force:
            print(f"  = {slug}: art exists, skipping (use --force)")
            continue
        try:
            data = gen(prompt, key)  # type: ignore[misc]
            dest.write_bytes(data)
            made += 1
            print(f"  + {slug}: {len(data)//1024} KB -> {dest.relative_to(AEON)}")
            time.sleep(0.4)  # gentle rate limiting
        except Exception as e:
            print(f"  ! {slug}: {e}", file=sys.stderr)

    if live:
        print(f"\nGenerated {made} illustration(s). "
              f"Re-run render_cards.py to composite them into the cards.")
    else:
        print("Set IMAGE_API + IMAGE_API_KEY and pass --go to generate for real.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
