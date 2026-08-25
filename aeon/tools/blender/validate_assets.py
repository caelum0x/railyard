#!/usr/bin/env python3
"""Validate that every champion spec has a resolvable, valid 3D asset.

Checks, for each `champions/specs/*.ron`:
  1. `assets/champions/<slug>/assets.manifest.ron` exists and parses.
  2. The manifest's `model` entry resolves to a file on disk.
  3. That file is a valid glTF binary: magic 'glTF' + version 2 + length.
  4. The file's BLAKE3 matches the hash recorded in the manifest (integrity).
  5. The manifest records a known archetype + faction.

Exit code 0 = all specs validate; non-zero = at least one failure (details on
stderr). Pure stdlib -- runnable with or without Blender.

Usage:
    python3 tools/blender/validate_assets.py [--specs DIR] [--assets DIR]
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

_THIS = Path(__file__).resolve()
_TOOLS_DIR = _THIS.parent.parent
_REPO_ROOT = _TOOLS_DIR.parent
if str(_TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(_TOOLS_DIR))

from aeonrift_assets import roster  # noqa: E402
from aeonrift_assets.blake3_pure import blake3_hex  # noqa: E402
from aeonrift_assets.manifest import gltf_header_ok, parse_manifest  # noqa: E402


def validate(specs_dir: Path, assets_dir: Path) -> list[str]:
    """Return a list of human-readable failure strings (empty == all good)."""
    failures: list[str] = []
    specs = list(roster.discover_specs(specs_dir))
    if not specs:
        return [f"no champion specs found in {specs_dir}"]

    for spec in specs:
        slug = spec["slug"]
        champ_dir = assets_dir / slug
        manifest_path = champ_dir / "assets.manifest.ron"

        if not manifest_path.exists():
            failures.append(f"{slug}: missing manifest {manifest_path}")
            continue

        man = parse_manifest(manifest_path)
        if man["archetype"] not in roster.ARCHETYPE_PROPORTIONS:
            failures.append(f"{slug}: unknown archetype {man['archetype']!r}")
        if man["faction"] not in roster.FACTION_COLORS:
            failures.append(f"{slug}: unknown faction {man['faction']!r}")

        model = man["assets"].get("model")
        if not model:
            failures.append(f"{slug}: manifest has no 'model' asset")
            continue

        model_path = champ_dir / model["path"]
        if not model_path.exists():
            failures.append(f"{slug}: model path does not resolve: {model_path}")
            continue

        data = model_path.read_bytes()
        ok, why = gltf_header_ok(data)
        if not ok:
            failures.append(f"{slug}: invalid glb ({why})")
            continue

        if model["bytes"] != len(data):
            failures.append(
                f"{slug}: manifest bytes {model['bytes']} != actual {len(data)}")
        actual_hash = blake3_hex(data)
        if model["blake3"] != actual_hash:
            failures.append(
                f"{slug}: blake3 mismatch (manifest {model['blake3'][:16]}... "
                f"!= actual {actual_hash[:16]}...)")

    return failures


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--specs", default=str(_REPO_ROOT / "champions" / "specs"))
    parser.add_argument("--assets", default=str(_REPO_ROOT / "assets" / "champions"))
    args = parser.parse_args(argv)

    specs_dir = Path(args.specs)
    assets_dir = Path(args.assets)

    failures = validate(specs_dir, assets_dir)
    n_specs = len(list(roster.discover_specs(specs_dir)))
    if failures:
        print(f"[validate_assets] {len(failures)} FAILURE(S):", file=sys.stderr)
        for f in failures:
            print(f"  ✗ {f}", file=sys.stderr)
        return 1
    print(f"[validate_assets] OK: all {n_specs} champion spec(s) resolve to a "
          f"valid glTF asset with a matching BLAKE3 hash.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
