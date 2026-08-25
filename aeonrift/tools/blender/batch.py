"""Batch-generate greybox character models for every champion spec.

Two run modes, auto-detected:

  * Under Blender  (`blender --background --python tools/blender/batch.py`):
    uses `gen_character` (bpy) to produce REAL rigged, metaball greyboxes.

  * Plain Python   (`python3 tools/blender/batch.py`):
    `bpy` is unavailable, so it falls back to the zero-dependency box-figure
    generator (`tools/gltf_fallback.py`). These are PLACEHOLDERS.

Either way it writes, per champion, under `assets/champions/<slug>/`:
    model.glb              the greybox model
    assets.manifest.ron    logical name -> path + format + bytes + BLAKE3

Archetype (proportions) and faction (tint) are resolved from the spec via
`aeonrift_assets.roster` (sidecar override -> inference).

Blender note: passing script args after `--`, e.g.
    blender --background --python tools/blender/batch.py -- --specs DIR --out DIR
"""

from __future__ import annotations

import sys
from pathlib import Path

# Resolve tools/ dir whether launched by python or by Blender.
_THIS = Path(__file__).resolve()
_TOOLS_DIR = _THIS.parent.parent
_REPO_ROOT = _TOOLS_DIR.parent
if str(_TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(_TOOLS_DIR))

from aeonrift_assets import roster  # noqa: E402
from aeonrift_assets.manifest import build_manifest_ron, make_asset_entry  # noqa: E402


def _parse_args():
    # Under Blender, real args come after a lone "--".
    argv = sys.argv
    if "--" in argv:
        argv = argv[argv.index("--") + 1:]
    else:
        argv = argv[1:] if not _under_blender() else []
    specs = _REPO_ROOT / "champions" / "specs"
    out = _REPO_ROOT / "assets" / "champions"
    it = iter(argv)
    for a in it:
        if a == "--specs":
            specs = Path(next(it))
        elif a == "--out":
            out = Path(next(it))
    return specs, out


def _under_blender() -> bool:
    try:
        import bpy  # noqa: F401
        return True
    except Exception:
        return False


def _generate_blender(specs_dir: Path, out_dir: Path):
    if str(_THIS.parent) not in sys.path:
        sys.path.insert(0, str(_THIS.parent))
    import gen_character  # type: ignore
    overrides = roster.load_overrides(specs_dir)
    results = []
    for spec in roster.discover_specs(specs_dir):
        meta = roster.resolve_meta(spec, overrides)
        model_path = gen_character.generate(
            spec["slug"], meta["archetype"], meta["faction"], out_dir
        )
        data = Path(model_path).read_bytes()
        entry = make_asset_entry("model.glb", data)
        manifest = build_manifest_ron(
            slug=spec["slug"], champion=spec["name"],
            archetype=meta["archetype"], faction=meta["faction"],
            generator=gen_character.GENERATOR, assets={"model": entry},
        )
        (out_dir / spec["slug"] / "assets.manifest.ron").write_text(
            manifest, encoding="utf-8")
        results.append(dict(slug=spec["slug"], archetype=meta["archetype"],
                            faction=meta["faction"], bytes=len(data),
                            generator=gen_character.GENERATOR))
    return results


def _generate_fallback(specs_dir: Path, out_dir: Path):
    import gltf_fallback
    raw = gltf_fallback.generate_all(specs_dir, out_dir)
    return [dict(slug=r["slug"], archetype=r["archetype"], faction=r["faction"],
                 bytes=r["bytes"], generator=gltf_fallback.GENERATOR) for r in raw]


def main():
    specs_dir, out_dir = _parse_args()
    out_dir.mkdir(parents=True, exist_ok=True)

    if _under_blender():
        print("[batch] Blender detected -> generating REAL rigged greyboxes (bpy).")
        results = _generate_blender(specs_dir, out_dir)
    else:
        print("[batch] Blender NOT available -> PLACEHOLDER box figures (fallback).")
        results = _generate_fallback(specs_dir, out_dir)

    print(f"[batch] specs: {specs_dir}")
    print(f"[batch] out:   {out_dir}")
    for r in results:
        print(f"  - {r['slug']:<12} {r['archetype']:<9} {r['faction']:<9} "
              f"{r['bytes']:>7} B  [{r['generator']}]")
    print(f"[batch] wrote {len(results)} champion model(s) + manifests.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
