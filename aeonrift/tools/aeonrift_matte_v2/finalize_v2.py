"""Ship the best variant per flagged champion + rewrite manifests (matte-v2 pass).

Per-target decision comes from out/qa_decisions_v2.json (set after the 3-way vision
compare). Each slug maps to exactly one of:
  * "hunyuan_v2"  -> ship the new matte-v2 Hunyuan mesh   (out/<slug>/model.gray.glb)
  * "hunyuan_prev"-> keep the previously-shipped Hunyuan model.glb (no mesh change)
  * "trellis"     -> ship the TRELLIS backup mesh          (model.trellis.glb)

For every target we ALWAYS:
  * back up the currently-shipped model.glb as model.hunyuan_v1.glb (once),
  * record the v2 mesh as model.hunyuan_v2.glb and its render as preview.hunyuan_v2.png,
  * keep model.trellis.glb (the TRELLIS backup — already present),
Then place the chosen model.glb + preview.png (+ preview.hunyuan.png for the gallery)
and rewrite assets-manifests/champions/<slug>/assets.manifest.ron with the correct
generator, model_source, fresh blake3 + bytes, and the reference entry.

Purely local file placement + manifest authoring; never regenerates anything.
"""
from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

import blake3

WT = Path("/Users/arhansubasi/products/rail-matte/aeonrift")
ASSETS = WT / "assets" / "champions"
MANIFESTS = WT / "assets-manifests" / "champions"
HERE = Path(__file__).resolve().parent
OUT = HERE / "out"
META = Path("/Users/arhansubasi/products/rail-matte/aeonrift/tools/aeonrift_3d_hq/out/champion_meta.json")

HUNYUAN_GENERATOR = "hunyuan3d-2.1"
TRELLIS_GENERATOR = "ai-3d"

TARGETS = [
    "cyranth", "kaeza", "korrathil", "mars", "veyl", "xelphine",
    "zeryth", "nyxralla", "petalsong", "artemis", "vulcan",
]


def b3(path: Path) -> str:
    h = blake3.blake3()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def render_manifest(slug: str, meta: dict, generator: str, model_source: str,
                    model_path: Path, ref_path: Path | None) -> str:
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
        f"            bytes: {model_path.stat().st_size},",
        f'            blake3: "{b3(model_path)}",',
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
    lines += ["    },", ")", ""]
    return "\n".join(lines)


def prev_model_source(slug: str) -> str:
    """Read the currently-committed manifest's model_source (the prior Hunyuan source)."""
    p = MANIFESTS / slug / "assets.manifest.ron"
    for line in p.read_text().splitlines():
        s = line.strip()
        if s.startswith("model_source:"):
            return s.split(":", 1)[1].strip().rstrip(",").strip().strip('"')
    return ""


def main() -> int:
    meta_all = json.loads(META.read_text())
    results = json.loads((OUT / "results_v2.json").read_text())
    v2_source = results["model_source"]
    decisions = json.loads((OUT / "qa_decisions_v2.json").read_text())

    summary = {"hunyuan_v2": [], "hunyuan_prev": [], "trellis": []}

    for slug in TARGETS:
        meta = meta_all[slug]
        cdir = ASSETS / slug
        model_glb = cdir / "model.glb"
        v2_glb = OUT / slug / "model.gray.glb"
        v2_prev = OUT / slug / "preview.gray.png"
        trellis_glb = cdir / "model.trellis.glb"
        trellis_prev = OUT / slug / "preview.trellis.png"

        decision = decisions[slug]

        # Back up the currently-shipped mesh once (the prior Hunyuan model.glb).
        v1_backup = cdir / "model.hunyuan_v1.glb"
        if model_glb.is_file() and not v1_backup.is_file():
            shutil.copy2(model_glb, v1_backup)
        # Record the v2 mesh + preview alongside (for provenance), when present.
        if v2_glb.is_file():
            shutil.copy2(v2_glb, cdir / "model.hunyuan_v2.glb")
        if v2_prev.is_file():
            shutil.copy2(v2_prev, cdir / "preview.hunyuan_v2.png")

        if decision == "hunyuan_v2":
            shutil.copy2(v2_glb, model_glb)
            shutil.copy2(v2_prev, cdir / "preview.png")
            shutil.copy2(v2_prev, cdir / "preview.hunyuan.png")
            generator, model_source = HUNYUAN_GENERATOR, v2_source
            summary["hunyuan_v2"].append(slug)
        elif decision == "trellis":
            shutil.copy2(trellis_glb, model_glb)
            if trellis_prev.is_file():
                shutil.copy2(trellis_prev, cdir / "preview.png")
            generator = TRELLIS_GENERATOR
            model_source = meta.get("trellis_model_source") or ""
            summary["trellis"].append(slug)
        else:  # hunyuan_prev — keep the currently-shipped mesh unchanged
            generator = HUNYUAN_GENERATOR
            model_source = prev_model_source(slug) or v2_source
            summary["hunyuan_prev"].append(slug)

        (MANIFESTS / slug).mkdir(parents=True, exist_ok=True)
        (MANIFESTS / slug / "assets.manifest.ron").write_text(
            render_manifest(slug, meta, generator, model_source,
                            model_glb, cdir / "reference.png"))

    (OUT / "finalize_v2_summary.json").write_text(json.dumps(summary, indent=2))
    print(f"hunyuan_v2 shipped: {len(summary['hunyuan_v2'])} -> {summary['hunyuan_v2']}")
    print(f"hunyuan_prev kept:  {len(summary['hunyuan_prev'])} -> {summary['hunyuan_prev']}")
    print(f"trellis shipped:    {len(summary['trellis'])} -> {summary['trellis']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
