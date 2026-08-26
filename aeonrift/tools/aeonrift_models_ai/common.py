"""Shared helpers for the Aeonrift AI 3D model pipeline.

SECRET HANDLING: the Replicate token is loaded from videogen/.env.local into the
process environment at runtime. It is NEVER printed, logged, or written to disk.
"""
from __future__ import annotations

import os
import re
from pathlib import Path

# The token lives in a sibling product's env file (owner-authorized).
ENV_FILE = Path(
    "/Users/arhansubasi/products/videogen/apps/web/.env.local"
)


def load_replicate_token() -> str:
    """Load REPLICATE_API_TOKEN into os.environ from the env file.

    Returns the token string for direct client use. Callers MUST NOT print it.
    Raises SystemExit with a clear (secret-free) message if missing/invalid.
    """
    tok = os.environ.get("REPLICATE_API_TOKEN")
    if tok and tok.startswith("r8_"):
        return tok
    if not ENV_FILE.is_file():
        raise SystemExit(f"STOP: env file not found at {ENV_FILE}")
    tok = None
    for line in ENV_FILE.read_text().splitlines():
        m = re.match(r"\s*(?:export\s+)?REPLICATE_API_TOKEN\s*=\s*(.+)\s*$", line)
        if m:
            tok = m.group(1).strip().strip('"').strip("'")
            break
    if not tok:
        raise SystemExit("STOP: REPLICATE_API_TOKEN not present in env file")
    if not tok.startswith("r8_"):
        raise SystemExit("STOP: REPLICATE_API_TOKEN has an unexpected format")
    os.environ["REPLICATE_API_TOKEN"] = tok
    return tok


# --- Paths -----------------------------------------------------------------
AEON = Path(__file__).resolve().parents[2]  # .../rail-3d/aeonrift
CHAMPS_CONTENT = AEON / "content" / "champions"
CHAMPS_ASSETS = AEON / "assets" / "champions"
MANIFESTS = AEON / "assets-manifests" / "champions"
ROSTER_META = CHAMPS_CONTENT / "roster_meta.ron"


def slugs() -> list[str]:
    out = []
    for p in sorted(CHAMPS_ASSETS.iterdir()):
        if p.is_dir() and (p / "model.glb").exists():
            out.append(p.name)
    return out


def roster_meta() -> dict[str, dict[str, str]]:
    text = ROSTER_META.read_text()
    meta: dict[str, dict[str, str]] = {}
    pat = re.compile(
        r'"([a-z0-9-]+)":\s*\(archetype:\s*"([a-z]+)",\s*faction:\s*"([A-Za-z]+)"\)'
    )
    for m in pat.finditer(text):
        meta[m.group(1)] = {"archetype": m.group(2), "faction": m.group(3)}
    return meta


def champion_name(slug: str) -> str:
    spec = CHAMPS_CONTENT / f"{slug}.ron"
    if spec.is_file():
        m = re.search(r'name:\s*"([^"]+)"', spec.read_text())
        if m:
            return m.group(1)
    return slug
