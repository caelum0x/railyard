#!/usr/bin/env python3
"""ANCHOR SET generator for the style-LoRA (League-painterly semi-realistic look).

Direction is LOCKED to ONE converged look: dramatic dynamic pose, moody cinematic
lighting with strong rim-light, atmospheric magical FX, ornate detail, painterly
semi-realistic -- produced by black-forest-labs/flux-1.1-pro (chosen after an A/B
vs recraft-v3: flux was more cinematic/rim-lit; recraft was flatter and stamped
fake signatures). ONE model + ONE style suffix held constant across the roster so
the pool reads as a single art-directed set to curate ~18 LoRA training images from.

Per champion the SUBJECT is auto-composed from:
  faction ENVIRONMENT + magical FX  x  archetype POSE  x  per-champion creature/weapon IDENTITY
so every candidate is distinct but on-style. 4:5 splash framing, NOT tight portrait.

FULLY HEADLESS: PNG files only. Token read at runtime from videogen/.env.local,
NEVER printed/committed. Resumable (skips existing PNGs). Honors 429 retry_after.

Output: assets/cards/style-test/<slug>__A_flux.png   (one converged look per champion)
"""
from __future__ import annotations
import json, sys, time, urllib.request, urllib.error, re, glob, os
from pathlib import Path

HERE = Path(__file__).resolve().parent
AEON = HERE.parent.parent
CHAMP = AEON / "content" / "champions"
OUT = AEON / "assets" / "cards" / "style-test"
OUT.mkdir(parents=True, exist_ok=True)
MANIFEST = OUT / "manifest.json"
ENV_LOCAL = Path("/Users/arhansubasi/products/videogen/apps/web/.env.local")
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"
MODEL = "black-forest-labs/flux-1.1-pro"

STYLE_SUFFIX = (
    "League of Legends splash art style, dynamic full-body action pose mid-motion, "
    "low dramatic camera angle, cinematic key-art composition, moody dark atmospheric "
    "environment, strong rim lighting with a glowing magical accent color, volumetric fog "
    "and floating particles, painterly semi-realistic digital illustration, ornate highly "
    "detailed fantasy armor and costume, rich cinematic color grade, sense of depth and "
    "atmosphere, tells a small story, epic fantasy concept art, artstation trending, "
    "highly detailed, sharp focus, no text, no watermark, no border"
)

FACTION_ENV = {
    "Greek":    "amid shattered white marble columns and golden divine light under storm clouds",
    "Norse":    "on a frozen fjord cliff beneath swirling aurora, driving snow and frost shards",
    "Roman":    "on a dark war-torn battlefield with legion banners and drifting embers",
    "Egyptian": "inside a torchlit desert tomb glowing with turquoise hieroglyphs and drifting sand",
    "Ember":    "across a dark volcanic ashland at night, molten fissures, fire and swirling cinders",
    "Tide":     "in sunken ruins deep in an ocean trench, ribbons of water and bioluminescent light",
    "Verdant":  "in a bioluminescent overgrown forest, drifting petals, spores and glowing green motes",
    "Umbral":   "in a starless void of purple eldritch energy, shadow tendrils and floating shards",
}
FACTION_ACCENT = {
    "Greek":    "ornate gold-and-white marble armor with blue-white lightning accents",
    "Norse":    "frost-steel and fur armor with icy-blue rune accents",
    "Roman":    "ornate crimson-and-bronze lorica armor with red-gold accents",
    "Egyptian": "ornate gold-and-obsidian regalia with turquoise soul-light accents",
    "Ember":    "scorched blackened-iron armor veined with lava and orange-red fire accents",
    "Tide":     "ornate teal-and-pearl armor with glowing cyan abyssal accents",
    "Verdant":  "living bark-and-leaf armor with emerald-green accents",
    "Umbral":   "dark voidmetal armor with violet-magenta accents",
}
ARCH_POSE = {
    "mage":     "conjuring surging magic mid-cast, arms sweeping wide as energy spirals around them, low dramatic angle",
    "bruiser":  "charging mid-leap into battle with weapon raised, dynamic diagonal motion, low heroic angle",
    "marksman": "loosing a glowing shot mid-motion with weapon drawn, agile dynamic stance, low angle",
    "assassin": "lunging from the shadows mid-strike as blades flash, explosive dynamic motion",
    "tank":     "bracing low and slamming into the ground sending out a shockwave, towering low-angle hero shot",
    "support":  "raising a radiant protective aura with cape and robes billowing, dramatic uplit angle",
}
IDENTITY = {
    "achilles": "legendary armored Greek hero of Troy with a plumed helm, round hoplite shield and spear",
    "anubis-warden-of-scales": "towering jackal-headed Egyptian death god holding golden scales of judgment",
    "artemis": "lithe Greek goddess of the hunt drawing a glowing crescent silver bow",
    "athena": "Greek goddess of wisdom in a gilded aegis with owl motifs, spear and shield",
    "bastet-claws-of-the-hunt": "sleek cat-headed Egyptian huntress with dual clawed gauntlets",
    "bellona": "Roman weapon-master goddess with a whip-blade and battle shield",
    "brackmoor": "hulking barnacle-crusted tide leviathan-knight with a massive shell shield",
    "cyranth": "sharp-eyed tide marksman wielding a coral longbow of glowing water",
    "dregmaw": "monstrous hulking Duskbulwark of jagged shadow plate, a void-tank",
    "freyja": "Norse enchantress goddess in a falcon-feather cloak weaving rune magic",
    "gorehoof": "brutish antlered Verdant beast-man swinging a mossy warclub, diving",
    "hades": "grim Greek lord of the underworld wreathed in dark blue soul-fire, black armor",
    "horus-skyfall-talon": "falcon-headed Egyptian sky-warrior with golden winged blades, diving down",
    "isis-mother-of-magic": "radiant winged Egyptian goddess with outstretched feathered wings and gold regalia",
    "janus": "two-faced Roman portal god with a double-faced helm summoning glowing gates",
    "kaeza": "ember markswoman the Emberlock aiming a molten flintlock rifle of fire",
    "korrathil": "monstrous many-fanged Voidgorger with dark tendrils and a gaping maw, diving",
    "loki": "grinning Norse trickster assassin with twin daggers and shimmering illusions",
    "lumisel": "graceful tide enchanter in luminous jellyfish-light robes",
    "mars": "disciplined Roman god of war with a plumed centurion helm and a flaming spear",
    "medusa": "serpent-haired gorgon markswoman with a petrifying glowing bow and a coiled snake tail",
    "mercury": "swift winged-sandaled Roman messenger assassin trailing a blur of speed",
    "mosscloak": "hulking Verdant treant tank cloaked in moss and bark with heavy stone fists",
    "neptune": "regal Roman sea-god raising a glowing trident amid surging tidal water",
    "nerreth": "shadowy ember Ashstalker assassin wielding blades of black fire",
    "nyxralla": "abyssal tide sorceress conjuring a swirling maelstrom of dark water",
    "odin": "one-eyed Norse allfather flanked by ravens, hurling the rune-crackling spear Gungnir",
    "petalsong": "delicate Verdant flower-spirit support scattering swirls of glowing petals",
    "pyromancer": "ember burst-mage hurling a massive fireball, robes trailing flame",
    "quorvane": "cloaked Umbral Nightglass Hunter aiming a crossbow of dark crystal",
    "ra-radiant-dawn": "radiant falcon-crowned Egyptian sun-god blazing with solar fire and a golden disc halo",
    "sentinel": "stoic Umbral automaton sentinel leveling a void-lance with a single glowing eye",
    "set-lord-of-chaos": "sinister beast-headed Egyptian god of chaos swinging a curved khopesh in a dark storm",
    "sporeveil": "eerie Verdant fungal mage cloaked in glowing mushrooms and drifting spore clouds",
    "thor": "mighty Norse storm-god swinging a lightning warhammer beneath thunderclouds",
    "thornquill": "Verdant archer drawing a living thorn-bow that fires glowing seed-arrows",
    "toran": "molten Slagwarden tank, a cracked stone golem body glowing with lava",
    "tyr": "stalwart one-handed Norse war-god raising a great shield and a rune-sword",
    "ullr": "frost-cloaked Norse god of the bow drawing an icy longbow in a winter storm",
    "vanguard": "armored Roman vanguard charging behind a tower shield with a short sword",
    "vashka": "feral ember Cindermaw beast-warrior with molten fangs charging through fire",
    "veyl": "shadowy tide assassin the Veyl wielding curved water-blades wreathed in mist",
    "vulcan": "burly Roman forge-god swinging a molten hammer amid sparks and lava",
    "xelphine": "mysterious Umbral Eclipse-Seer mage with a floating dark sun and violet runes",
    "ysmera": "ember Pyre-Oracle sorceress wreathed in prophetic flame",
    "zeryth": "sleek Umbral Fang assassin striking with twin void-daggers and shadow",
    "zeus": "mighty bearded Greek storm-god hurling a forked lightning bolt",
}

def champ_meta():
    rows = []
    for meta in sorted(glob.glob(str(CHAMP / "*" / "card_meta.ron"))):
        slug = os.path.basename(os.path.dirname(meta))
        t = open(meta).read()
        def g(k):
            m = re.search(rf'{k}:\s*"([^"]+)"', t); return m.group(1) if m else ""
        rows.append((slug, g("display_name"), g("faction"), g("archetype")))
    return rows

def build_prompt(slug, disp, fac, arch):
    ident = IDENTITY.get(slug, f"a mighty armored {fac} champion named {disp}")
    pose = ARCH_POSE.get(arch, ARCH_POSE["bruiser"])
    env = FACTION_ENV.get(fac, "in an epic fantasy environment")
    accent = FACTION_ACCENT.get(fac, "ornate detailed armor")
    subject = f"{disp}, {ident}, {pose}, {env}, wearing {accent}"
    return f"{subject}. {STYLE_SUFFIX}"

# ---- API ----
def load_token():
    for line in ENV_LOCAL.read_text().splitlines():
        if line.startswith("REPLICATE_API_TOKEN"):
            t = line.split("=", 1)[1].strip().strip('"').strip("'")
            if t.startswith("r8_"):
                return t
    raise SystemExit("token not found")
TOKEN = load_token()

def _h(extra=None):
    h = {"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json", "User-Agent": UA}
    if extra: h.update(extra)
    return h

_last = [0.0]
GAP = 11.0
def create(model, inp):
    url = f"https://api.replicate.com/v1/models/{model}/predictions"
    body = json.dumps({"input": inp}).encode()
    while True:
        d = GAP - (time.time() - _last[0])
        if d > 0: time.sleep(d)
        req = urllib.request.Request(url, data=body, headers=_h({"Prefer": "wait"}), method="POST")
        try:
            with urllib.request.urlopen(req, timeout=180) as r:
                _last[0] = time.time(); return json.load(r)
        except urllib.error.HTTPError as e:
            _last[0] = time.time()
            if e.code == 429:
                try: wait = float(json.loads(e.read().decode()).get("retry_after", 6)) + 1
                except Exception: wait = 8
                print(f"    429; wait {wait:.0f}s", flush=True); time.sleep(wait); continue
            raise RuntimeError(f"HTTP {e.code}: {e.read().decode()[:200]}")

def poll(p):
    if p.get("status") in ("succeeded", "failed", "canceled"): return p
    gu = (p.get("urls") or {}).get("get")
    for _ in range(120):
        time.sleep(3)
        with urllib.request.urlopen(urllib.request.Request(gu, headers=_h()), timeout=60) as r:
            p = json.load(r)
        if p.get("status") in ("succeeded", "failed", "canceled"): return p
    return p

def first_url(o):
    if isinstance(o, str): return o
    if isinstance(o, list) and o: return o[0]
    return None

def download(u, dest):
    with urllib.request.urlopen(urllib.request.Request(u, headers={"User-Agent": UA}), timeout=180) as r:
        dest.write_bytes(r.read())

def main():
    manifest = json.loads(MANIFEST.read_text()) if MANIFEST.exists() else {}
    rows = champ_meta()
    total = len(rows); ok = 0
    for i, (slug, disp, fac, arch) in enumerate(rows, 1):
        dest = OUT / f"{slug}__A_flux.png"
        prompt = build_prompt(slug, disp, fac, arch)
        rec = manifest.setdefault(slug, {}).setdefault("A_flux", {})
        rec.update({"model": MODEL, "prompt": prompt, "faction": fac, "archetype": arch, "display": disp})
        if dest.exists() and dest.stat().st_size > 1000:
            print(f"[{i}/{total} skip] {dest.name}", flush=True)
            rec["file"] = dest.name; ok += 1
            MANIFEST.write_text(json.dumps(manifest, indent=2)); continue
        print(f"[{i}/{total} gen ] {slug} ({fac}/{arch})", flush=True)
        try:
            p = poll(create(MODEL, {"prompt": prompt, "aspect_ratio": "4:5",
                                    "output_format": "png", "prompt_upsampling": True,
                                    "safety_tolerance": 5}))
        except Exception as e:
            print(f"    ERROR: {e}", flush=True); rec["error"] = str(e)[:200]
            MANIFEST.write_text(json.dumps(manifest, indent=2)); continue
        if p.get("status") != "succeeded":
            print(f"    FAILED: {p.get('error') or p.get('status')}", flush=True)
            rec["error"] = str(p.get("error") or p.get("status"))[:200]
            MANIFEST.write_text(json.dumps(manifest, indent=2)); continue
        u = first_url(p.get("output"))
        download(u, dest)
        rec["file"] = dest.name; rec["output_url"] = u; rec.pop("error", None); ok += 1
        print(f"    saved {dest.name} ({dest.stat().st_size//1024} KB)", flush=True)
        MANIFEST.write_text(json.dumps(manifest, indent=2))
    print(f"DONE {ok}/{total} flux anchors present", flush=True)

if __name__ == "__main__":
    main()
