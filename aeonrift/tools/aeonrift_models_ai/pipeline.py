"""Aeonrift AI 3D champion model pipeline (FULLY HEADLESS).

Per champion:
  a) Flux (black-forest-labs/flux-dev) -> reference.png  (full-body 3D-ready ref)
  b) firtoz/trellis (image-to-3D)      -> model.glb       (textured, replaces greybox)
  c) Blender EEVEE headless render      -> preview.png     (inspectable, no window)

Incremental + retry/skip: partial progress survives; failures are logged, not fatal.
The Replicate token is loaded via common.load_replicate_token() and never printed.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

import replicate

import common

FLUX_MODEL = "black-forest-labs/flux-dev"
TRELLIS_MODEL = "firtoz/trellis"

PROMPT_SUFFIX = (
    "full body character concept, T-pose, front view, neutral background, "
    "game character, stylized fantasy, clean silhouette, even lighting, no text"
)

# Faction -> flavor to steer Flux toward the right palette/mood.
FACTION_FLAVOR = {
    "Greek": "ancient Greek mythology, gold and marble, olympian",
    "Norse": "norse mythology, steel-blue, runes and fur, frost",
    "Roman": "ancient Roman, crimson and bronze, legionary",
    "Egyptian": "ancient Egyptian mythology, sand-gold, hieroglyphs",
    "Ember": "volcanic ember, molten orange and black, fire",
    "Tide": "deep-sea tide, cyan and teal, water and coral",
    "Verdant": "verdant forest, mossy green, nature and vines",
    "Umbral": "umbral shadow, violet and dark purple, eldritch",
}

# Slug -> mythic/fantasy identity for the SUBJECT of the concept art.
IDENTITY = {
    "zeus": "Zeus the storm god king in full ornate golden armor and a flowing toga robe, fully clothed, wielding lightning, regal white beard, majestic",
    "hades": "Hades god of the underworld, dark armor, ghostly crown",
    "athena": "Athena goddess of wisdom and war, owl, spear and aegis",
    "medusa": "Medusa the gorgon, snake hair, serpent lower body, bow",
    "achilles": "Achilles the greek hero warrior, bronze armor, spear",
    "artemis": "Artemis goddess of the hunt, silver bow, crescent moon",
    "tyr": "Tyr the norse god of war and justice, one hand, heavy shield",
    "thor": "Thor the norse thunder god, war hammer Mjolnir, red cape",
    "odin": "Odin the allfather, one eye, ravens, gungnir spear, cloak",
    "loki": "Loki the trickster god, green and gold, daggers, sly grin",
    "freyja": "Freyja norse goddess of love and war, falcon cloak, gold",
    "ullr": "Ullr norse god of archery and skiing, longbow, winter furs",
    "mars": "Mars the roman god of war, crimson plumed helm, gladius",
    "vulcan": "Vulcan roman god of the forge, hammer and anvil, molten",
    "bellona": "Bellona roman goddess of war, dual blades, red banner",
    "neptune": "Neptune roman god of the sea, trident, aqua armor",
    "mercury": "Mercury roman messenger god, winged sandals, caduceus",
    "janus": "Janus roman two-faced god of doorways and time, twin faces",
    "ra-radiant-dawn": "Ra the egyptian sun god, falcon head, solar disk",
    "anubis-warden-of-scales": "Anubis jackal-headed egyptian death god, scales of judgment",
    "horus-skyfall-talon": "Horus falcon-headed egyptian sky god, khopesh, wings",
    "isis-mother-of-magic": "Isis egyptian goddess of magic, winged arms, ankh staff",
    "set-lord-of-chaos": "Set egyptian god of chaos, beast head, red desert, was-scepter",
    "bastet-claws-of-the-hunt": "Bastet egyptian cat goddess, feline warrior, claws",
    "toran": "Toran an ember fire mage, molten robes, flame staff",
    "vashka": "Vashka an ember bruiser warrior, obsidian armor, ash",
    "ysmera": "Ysmera an ember sorceress, burning veil, cinder magic",
    "kaeza": "Kaeza an ember marksman ranger, flaming crossbow",
    "nerreth": "Nerreth an ember brute, cracked lava skin, greatsword",
    "brackmoor": "Brackmoor a tide sea-bruiser, barnacle armor, harpoon",
    "nyxralla": "Nyxralla a tide deep-sea sorceress, jellyfish veil, staff",
    "cyranth": "Cyranth a tide marksman, coral bow, flowing water arrows",
    "veyl": "Veyl a tide assassin, eel-like, twin daggers, mist",
    "lumisel": "Lumisel a tide luminous support healer, glowing pearls",
    "mosscloak": "Mosscloak a verdant forest bruiser, bark armor, cudgel",
    "gorehoof": "Gorehoof a verdant minotaur bruiser, antlers, war-maul",
    "sporeveil": "Sporeveil a verdant mushroom mage, spore cloud, fungal staff",
    "thornquill": "Thornquill a verdant marksman, thorn-vine bow, quills",
    "petalsong": "Petalsong a verdant support druid, flower crown, harp",
    "dregmaw": "Dregmaw an umbral shadow tank, void carapace, huge maw",
    "korrathil": "Korrathil an umbral bruiser, tentacled armor, dark blade",
    "xelphine": "Xelphine an umbral void mage, floating shards, violet fire",
    "quorvane": "Quorvane an umbral marksman, shadow rifle, cloaked",
    "zeryth": "Zeryth an umbral assassin, wraith form, curved shadow knives",
    "sentinel": "a futuristic umbral marksman sentinel, energy rifle, visor",
    "pyromancer": "an ember pyromancer battlemage, fire orbs, molten cloak",
    "vanguard": "a roman vanguard tank knight, tower shield, plate armor",
}

RENDER_SCRIPT = Path(__file__).resolve().parent / "render_preview.py"

# Replicate throttles prediction creation to ~6/min (burst 1) when balance < $5.
# Keep >= 12s between CREATE calls, and retry 429s with exponential backoff.
MIN_CREATE_INTERVAL = 12.0
_last_create = [0.0]


def _throttle() -> None:
    now = time.monotonic()
    wait = MIN_CREATE_INTERVAL - (now - _last_create[0])
    if wait > 0:
        time.sleep(wait)
    _last_create[0] = time.monotonic()


def with_retry(fn, what: str, max_tries: int = 6):
    """Run a Replicate CREATE call with pacing + 429/5xx backoff."""
    delay = 15.0
    last = None
    for attempt in range(1, max_tries + 1):
        _throttle()
        try:
            return fn()
        except Exception as e:  # noqa: BLE001
            msg = str(e)
            last = e
            transient = ("429" in msg or "throttl" in msg.lower()
                         or "rate limit" in msg.lower() or "502" in msg or "503" in msg)
            if not transient or attempt == max_tries:
                raise
            print(f"    [{what}] transient ({msg[:60]}...), retry {attempt}/{max_tries} in {delay:.0f}s", flush=True)
            time.sleep(delay)
            delay = min(delay * 1.6, 90.0)
    raise last  # pragma: no cover


def log(champ_dir: Path, msg: str) -> None:
    line = f"[{time.strftime('%H:%M:%S')}] {msg}"
    print(line, flush=True)
    with (champ_dir / "pipeline.log").open("a") as f:
        f.write(line + "\n")


def blake3_hex(path: Path) -> str:
    # Prefer blake3 if available; fall back is not needed since manifests use blake3.
    try:
        from blake3 import blake3 as _b3  # type: ignore

        h = _b3()
        with path.open("rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()
    except Exception:
        # Compute via a subprocess to the `b3sum` tool if present, else sha256 marker.
        b3 = shutil.which("b3sum")
        if b3:
            out = subprocess.check_output([b3, "--no-names", str(path)]).decode()
            return out.strip().split()[0]
        h = hashlib.sha256()
        with path.open("rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return "sha256:" + h.hexdigest()


def is_valid_glb(path: Path) -> bool:
    try:
        with path.open("rb") as f:
            magic = f.read(4)
        return magic == b"glTF"
    except Exception:
        return False


def build_prompt(slug: str, meta: dict) -> str:
    identity = IDENTITY.get(slug, f"a {meta.get('archetype','warrior')} fantasy champion")
    flavor = FACTION_FLAVOR.get(meta.get("faction", ""), "stylized fantasy")
    return f"{identity}, {flavor}, {PROMPT_SUFFIX}"


def gen_reference(client: replicate.Client, slug: str, meta: dict, out: Path) -> bool:
    if out.exists() and out.stat().st_size > 1000:
        return True
    prompt = build_prompt(slug, meta)
    result = with_retry(lambda: client.run(
        FLUX_MODEL,
        input={
            "prompt": prompt,
            "aspect_ratio": "3:4",
            "num_outputs": 1,
            "output_format": "png",
            "output_quality": 95,
            "num_inference_steps": 28,
            "guidance": 3.5,
            "disable_safety_checker": False,
        },
    ), "flux")
    item = result[0] if isinstance(result, list) else result
    data = item.read() if hasattr(item, "read") else None
    if data is None:
        return False
    out.write_bytes(data)
    return out.exists() and out.stat().st_size > 1000


def gen_model(client: replicate.Client, ref: Path, out: Path) -> tuple[bool, str]:
    """Run TRELLIS image-to-3D. Returns (ok, version_ref)."""
    model = client.models.get(TRELLIS_MODEL)
    version = model.versions.list()[0]
    def _create():
        with ref.open("rb") as fh:
            return client.predictions.create(
                version=version.id,
                input={
                    "images": [fh],
                    "generate_model": True,
                    "generate_color": False,
                    "generate_normal": False,
                    "texture_size": 1024,
                    "mesh_simplify": 0.9,
                    "ss_sampling_steps": 12,
                    "slat_sampling_steps": 12,
                },
            )

    prediction = with_retry(_create, "trellis")
    prediction.wait()
    if prediction.status != "succeeded":
        raise RuntimeError(f"trellis status={prediction.status} err={str(prediction.error)[:160]}")
    output = prediction.output
    # output is a dict; the GLB is under 'model_file'
    url = None
    if isinstance(output, dict):
        url = output.get("model_file")
    elif isinstance(output, str):
        url = output
    if not url:
        raise RuntimeError(f"trellis returned no model_file: keys={list(output) if isinstance(output,dict) else type(output)}")
    # download
    import urllib.request

    tmp = out.with_suffix(".glb.tmp")
    urllib.request.urlretrieve(url, tmp)
    if not is_valid_glb(tmp):
        tmp.unlink(missing_ok=True)
        raise RuntimeError("downloaded file is not a valid GLB (bad magic)")
    tmp.replace(out)
    return True, f"{TRELLIS_MODEL}:{version.id}"


def render_preview(model_glb: Path, out_png: Path) -> bool:
    if not shutil.which("blender"):
        return False
    cmd = [
        "blender", "--background", "--factory-startup",
        "--python", str(RENDER_SCRIPT), "--",
        str(model_glb), str(out_png),
    ]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
    ok = out_png.exists() and out_png.stat().st_size > 1000
    if not ok:
        (out_png.parent / "render_error.log").write_text(
            (r.stdout or "")[-4000:] + "\n---STDERR---\n" + (r.stderr or "")[-4000:]
        )
    return ok


def write_manifest(slug: str, meta: dict, model_glb: Path, model_used: str) -> None:
    name = common.champion_name(slug)
    b3 = blake3_hex(model_glb)
    size = model_glb.stat().st_size
    mdir = common.MANIFESTS / slug
    mdir.mkdir(parents=True, exist_ok=True)
    content = f'''AssetManifest(
    slug: "{slug}",
    champion: "{name}",
    archetype: "{meta.get('archetype','')}",
    faction: "{meta.get('faction','')}",
    generator: "ai-3d",
    model_source: "{model_used}",
    assets: {{
        "model": AssetEntry(
            path: "model.glb",
            format: "model/gltf-binary",
            bytes: {size},
            blake3: "{b3}",
        ),
        "reference": AssetEntry(
            path: "reference.png",
            format: "image/png",
            bytes: {(model_glb.parent / "reference.png").stat().st_size if (model_glb.parent / "reference.png").exists() else 0},
            blake3: "",
        ),
    }},
)
'''
    (mdir / "assets.manifest.ron").write_text(content)


def process(slug: str, client: replicate.Client, meta_all: dict, force: bool) -> dict:
    cdir = common.CHAMPS_ASSETS / slug
    meta = meta_all.get(slug, {})
    result = {"slug": slug, "reference": False, "model": False, "preview": False, "error": ""}
    try:
        ref = cdir / "reference.png"
        if not gen_reference(client, slug, meta, ref):
            raise RuntimeError("flux reference generation failed")
        result["reference"] = True
        log(cdir, f"{slug}: reference.png ok")

        model_glb = cdir / "model.glb"
        greybox_backup = cdir / "model.greybox.glb"
        already_ai = (cdir / ".ai3d.done").exists()
        if already_ai and not force:
            result["model"] = True
            result["preview"] = (cdir / "preview.png").exists()
            log(cdir, f"{slug}: already ai-3d, skipping model gen")
            return result
        # backup greybox once (only if current model isn't already ai-generated)
        if not greybox_backup.exists() and model_glb.exists():
            shutil.copy2(model_glb, greybox_backup)
        ok, model_used = gen_model(client, ref, model_glb)
        if not ok:
            raise RuntimeError("trellis model generation failed")
        result["model"] = True
        log(cdir, f"{slug}: model.glb ({model_glb.stat().st_size} bytes) via {model_used}")

        write_manifest(slug, meta, model_glb, model_used)
        (cdir / ".ai3d.done").write_text(model_used + "\n")

        preview = cdir / "preview.png"
        if render_preview(model_glb, preview):
            result["preview"] = True
            log(cdir, f"{slug}: preview.png ok")
        else:
            log(cdir, f"{slug}: preview render FAILED (see render_error.log)")
    except Exception as e:  # noqa: BLE001
        result["error"] = f"{type(e).__name__}: {str(e)[:200]}"
        log(cdir, f"{slug}: ERROR {result['error']}")
    return result


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", nargs="*", help="process only these slugs")
    ap.add_argument("--force", action="store_true", help="regenerate even if .ai3d.done")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    tok = common.load_replicate_token()
    client = replicate.Client(api_token=tok)
    meta_all = common.roster_meta()
    all_slugs = common.slugs()
    todo = args.only if args.only else all_slugs
    if args.limit:
        todo = todo[: args.limit]

    summary = []
    for i, slug in enumerate(todo, 1):
        print(f"\n=== ({i}/{len(todo)}) {slug} ===", flush=True)
        summary.append(process(slug, client, meta_all, args.force))

    # write summary json
    sp = Path(__file__).resolve().parent / "summary.json"
    sp.write_text(json.dumps(summary, indent=2))
    ok = sum(1 for s in summary if s["model"])
    prev = sum(1 for s in summary if s["preview"])
    fail = [s for s in summary if not s["model"]]
    print(f"\n=== DONE: {ok}/{len(todo)} models, {prev} previews, {len(fail)} failed ===")
    for s in fail:
        print(f"  FAIL {s['slug']}: {s['error']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
