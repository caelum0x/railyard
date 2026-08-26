"""Place shipped assets + rewrite manifests after generation and QA.

For each champion, decide (via out/qa_decisions.json; default = hunyuan) whether the
shipped model.glb is the new Hunyuan3D-2.1 mesh or the TRELLIS fallback, then:

  * copy reference.png into the worktree champion dir,
  * keep the TRELLIS mesh as model.trellis.glb (backup),
  * write the chosen mesh as model.glb,
  * copy the Hunyuan preview render (preview.hunyuan.png) for the gallery, and the
    shipped model's preview as preview.png,
  * rewrite assets-manifests/champions/<slug>/assets.manifest.ron with the correct
    generator, model_source, fresh blake3 + bytes, and a reference entry.

Never re-generates anything; purely local file placement + manifest authoring.
"""
from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

import blake3

WT = Path("/Users/arhansubasi/products/rail-3d-hq/aeonrift")
SRC_BASE = Path("/Users/arhansubasi/products/rail-3d/aeonrift/assets/champions")
HERE = Path(__file__).resolve().parent
OUT = HERE / "out"

ASSETS = WT / "assets" / "champions"
MANIFESTS = WT / "assets-manifests" / "champions"

HUNYUAN_GENERATOR = "hunyuan3d-2.1"
TRELLIS_GENERATOR = "ai-3d"


def b3(path: Path) -> str:
    h = blake3.blake3()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def render_manifest(slug: str, meta: dict, generator: str, model_source: str,
                    model_path: Path, ref_path: Path | None) -> str:
    model_bytes = model_path.stat().st_size
    model_hash = b3(model_path)
    lines = [
        "AssetManifest(",
        f'    slug: "{slug}",',
        f'    champion: "{meta["champion"]}",',
        f'    archetype: "{meta["archetype"]}",',
        f'    faction: "{meta["faction"]}",',
        f'    generator: "{generator}",',
        f'    model_source: "{model_source}",',
        "    assets: {",
        '        "model": AssetEntry(',
        '            path: "model.glb",',
        '            format: "model/gltf-binary",',
        f"            bytes: {model_bytes},",
        f'            blake3: "{model_hash}",',
        "        ),",
    ]
    if ref_path and ref_path.is_file():
        lines += [
            '        "reference": AssetEntry(',
            '            path: "reference.png",',
            '            format: "image/png",',
            f"            bytes: {ref_path.stat().st_size},",
            f'            blake3: "{b3(ref_path)}",',
            "        ),",
        ]
    lines += [
        "    },",
        ")",
        "",
    ]
    return "\n".join(lines)


def main() -> int:
    results = json.loads((OUT / "results.json").read_text())
    meta_all = json.loads((OUT / "champion_meta.json").read_text())
    hunyuan_source = results["model_source"]

    decisions_path = OUT / "qa_decisions.json"
    decisions = {}
    if decisions_path.is_file():
        decisions = json.loads(decisions_path.read_text())

    summary = {"hunyuan": [], "trellis_fallback": [], "skipped": []}

    for slug, meta in sorted(meta_all.items()):
        rec = results["champions"].get(slug, {})
        champ_dir = ASSETS / slug
        champ_dir.mkdir(parents=True, exist_ok=True)
        src_ref = SRC_BASE / slug / "reference.png"
        src_trellis = SRC_BASE / slug / "model.glb"
        src_trellis_preview = SRC_BASE / slug / "preview.png"
        hunyuan_glb = OUT / slug / "model.hunyuan.glb"
        hunyuan_preview = OUT / slug / "preview.png"

        # reference.png (always ship the concept ref)
        if src_ref.is_file():
            shutil.copy2(src_ref, champ_dir / "reference.png")

        # TRELLIS backup (always)
        if src_trellis.is_file():
            shutil.copy2(src_trellis, champ_dir / "model.trellis.glb")

        # Hunyuan preview for gallery (always, when available)
        if hunyuan_preview.is_file():
            shutil.copy2(hunyuan_preview, champ_dir / "preview.hunyuan.png")

        decision = decisions.get(slug)
        hunyuan_ok = rec.get("ok") and hunyuan_glb.is_file()
        if decision is None:
            decision = "hunyuan" if hunyuan_ok else "trellis"
        if decision == "hunyuan" and not hunyuan_ok:
            decision = "trellis"

        if decision == "hunyuan":
            shutil.copy2(hunyuan_glb, champ_dir / "model.glb")
            if hunyuan_preview.is_file():
                shutil.copy2(hunyuan_preview, champ_dir / "preview.png")
            generator, model_source = HUNYUAN_GENERATOR, hunyuan_source
            summary["hunyuan"].append(slug)
        else:
            if not src_trellis.is_file():
                summary["skipped"].append(slug)
                continue
            shutil.copy2(src_trellis, champ_dir / "model.glb")
            if src_trellis_preview.is_file():
                shutil.copy2(src_trellis_preview, champ_dir / "preview.png")
            generator = TRELLIS_GENERATOR
            model_source = meta.get("trellis_model_source") or ""
            summary["trellis_fallback"].append(slug)

        manifest_dir = MANIFESTS / slug
        manifest_dir.mkdir(parents=True, exist_ok=True)
        (manifest_dir / "assets.manifest.ron").write_text(
            render_manifest(slug, meta, generator, model_source,
                            champ_dir / "model.glb", champ_dir / "reference.png"))

    (OUT / "finalize_summary.json").write_text(json.dumps(summary, indent=2))
    print(f"hunyuan-shipped: {len(summary['hunyuan'])}")
    print(f"trellis-fallback: {len(summary['trellis_fallback'])} -> {summary['trellis_fallback']}")
    if summary["skipped"]:
        print(f"SKIPPED (no asset): {summary['skipped']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
