"""Parse Aeonrift champion data from RON sidecars and build cohesive Flux prompts.

Data-only: reads content/champions/<slug>/{card_meta.ron,meta.ron} plus the flat
content/champions/<slug>.ron for the signature ability. No RON loader dependency --
the sidecars are simple enough to scrape with regex.
"""
from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path

# Repo layout: this file lives at <aeonrift>/assets/cards/champions.py
AEONRIFT = Path(__file__).resolve().parents[2]
CHAMPS = AEONRIFT / "content" / "champions"

# Cohesive style suffix -- used VERBATIM for every card so the whole set matches.
STYLE_SUFFIX = (
    "epic mythological character splash art, dramatic cinematic lighting, "
    "painterly dark-fantasy, ornate, highly detailed, centered portrait, "
    "trading card game key art, artstation trending, no text, no border"
)

# Faction -> (accent hex, tint word for the prompt) from roster_meta.ron notes.
FACTION_COLOR = {
    "Greek":    ("#E8C15A", "radiant gold and marble"),
    "Norse":    ("#6FA8D6", "frostbitten steel-blue and iron"),
    "Roman":    ("#C6413B", "imperial crimson and bronze"),
    "Egyptian": ("#D9A94A", "sand-gold and lapis, hieroglyph motifs"),
    "Ember":    ("#E8772E", "molten orange and ember embers"),
    "Tide":     ("#3FB6C4", "deep cyan and abyssal teal"),
    "Verdant":  ("#5DA84E", "verdant green and living wood"),
    "Umbral":   ("#8B5CC7", "violet shadow and void purple"),
}
FACTION_ORDER = ["Greek", "Norse", "Roman", "Egyptian", "Ember", "Tide", "Verdant", "Umbral"]

# Rarity -> (frame accent hex, gem hex) -- classic TCG tiering.
RARITY_COLOR = {
    "Common":    ("#9AA3AD", "#C7CDD4"),
    "Rare":      ("#4C86D6", "#6FA8FF"),
    "Epic":      ("#A45CD6", "#C77DFF"),
    "Legendary": ("#E8A93A", "#FFD86B"),
}

ARCHETYPE_DESC = {
    "tank":     "hulking armored guardian, immovable bulwark",
    "bruiser":  "battle-scarred frontline warrior, brutal melee",
    "mage":     "arcane spellcaster wreathed in magic energy",
    "marksman": "deadly ranged hunter, poised to strike",
    "assassin": "lithe shadow-stalker, lethal and swift",
    "support":  "serene guardian channeling protective power",
}

# Curated domain descriptors for named mythological gods so identity reads true.
DOMAIN = {
    "zeus": "king of the gods, god of lightning and sky, crackling storm, golden aura",
    "hades": "grim god of the underworld, dark crown, ghostly green fire",
    "athena": "goddess of wisdom and war, gleaming aegis, owl, spear and shield",
    "medusa": "serpent-haired gorgon, petrifying green stare, coiling snakes",
    "achilles": "invincible hero of Troy, bronze spear and shield, immortal rage",
    "artemis": "goddess of the hunt and moon, silver bow, lunar glow",
    "tyr": "one-handed norse god of war and justice, rune-etched sword",
    "thor": "god of thunder, mighty hammer Mjolnir, storm lightning",
    "odin": "allfather, one-eyed norse god, ravens, gungnir spear, runic power",
    "loki": "trickster god, mischievous grin, shifting green flame, daggers",
    "freyja": "norse goddess of love and war, falcon cloak, seidr magic",
    "ullr": "norse god of the hunt and archery, frost bow, skis of ice",
    "mars": "roman god of war, crimson legionnaire armor, blood-red gladius",
    "vulcan": "roman god of the forge, molten hammer, anvil sparks, lava",
    "bellona": "roman goddess of war, spear and shield, crested helm",
    "neptune": "roman god of the sea, trident, churning ocean, coral crown",
    "mercury": "roman messenger god, winged sandals, caduceus, blur of speed",
    "janus": "two-faced roman god of doorways and time, twin visages, portal",
    "ra-radiant-dawn": "egyptian sun god, falcon head, blazing solar disk, radiant dawn",
    "anubis-warden-of-scales": "jackal-headed egyptian god of the dead, obsidian and gold, scales of judgment",
    "horus-skyfall-talon": "falcon-headed egyptian sky god, golden wings, eye of horus",
    "isis-mother-of-magic": "egyptian goddess of magic, outstretched feathered wings, ankh, starlight",
    "set-lord-of-chaos": "egyptian god of chaos and storms, crimson eyes, was-scepter, desert tempest",
    "bastet-claws-of-the-hunt": "cat-headed egyptian goddess, sleek predator, gold jewelry, moonlit hunt",
}

RARITY_RANK = {"Common": 0, "Rare": 1, "Epic": 2, "Legendary": 3}


@dataclass
class Champion:
    slug: str
    name: str
    title: str
    faction: str
    archetype: str
    rarity: str
    cost: int
    hp: int
    ad: int
    armor: int
    rng: float
    lore: str
    ability_name: str
    ability_line: str
    accent: str = ""
    tint: str = ""
    rarity_accent: str = ""
    gem: str = ""

    def prompt(self) -> str:
        subject = DOMAIN.get(self.slug)
        if not subject:
            # Fantasy champions: derive from lore body (strip "Name — ROLE." prefix).
            body = re.sub(r"^[^.]*\.\s*", "", self.lore).strip()
            body = re.sub(r"\s+", " ", body)
            if len(body) > 200:
                body = body[:200].rsplit(" ", 1)[0]
            subject = body or self.name
        arche = ARCHETYPE_DESC.get(self.archetype, "mythic champion")
        _, tintword = FACTION_COLOR.get(self.faction, ("", "mythic"))
        name_part = self.name if not self.title else f"{self.name}, {self.title}"
        return (
            f"{name_part}, {subject}, {arche}, "
            f"{tintword} color palette, {STYLE_SUFFIX}"
        )


def _grab(text: str, key: str) -> str | None:
    m = re.search(rf'{key}\s*:\s*"([^"]*)"', text)
    return m.group(1) if m else None


def _grab_num(text: str, key: str) -> str | None:
    m = re.search(rf"{key}\s*:\s*([0-9.]+)", text)
    return m.group(1) if m else None


def _ability_line(flat: str) -> tuple[str, str]:
    """First AbilitySpec: name + short effect summary."""
    m = re.search(r"AbilitySpec\(\s*name:\s*\"([^\"]+)\"(.*?)\n\s*\),", flat, re.S)
    if not m:
        return ("Signature Ability", "")
    name, block = m.group(1), m.group(2)
    parts = []
    dmg = re.search(r"Damage\(amount:\s*(\d+)", block)
    if dmg:
        parts.append(f"{dmg.group(1)} damage")
    if "Stun(" in block:
        parts.append("stun")
    if "Dash(" in block:
        parts.append("dash")
    if "Slow(" in block:
        parts.append("slow")
    if re.search(r"Heal\(", block):
        parts.append("heal")
    if "Shield(" in block:
        parts.append("shield")
    if "Knockback" in block or "Knockup" in block:
        parts.append("knockback")
    return (name, " · ".join(parts))


def load_all() -> list[Champion]:
    champs: list[Champion] = []
    for folder in sorted(p for p in CHAMPS.iterdir() if p.is_dir()):
        slug = folder.name
        cm = folder / "card_meta.ron"
        mm = folder / "meta.ron"
        flat = CHAMPS / f"{slug}.ron"
        if not cm.exists():
            continue
        cmt = cm.read_text()
        mmt = mm.read_text() if mm.exists() else ""
        flatt = flat.read_text() if flat.exists() else ""

        rarity = _grab(cmt, "rarity") or "Common"
        faction = _grab(cmt, "faction") or "Umbral"
        accent, tint = FACTION_COLOR.get(faction, ("#888888", "mythic"))
        r_accent, gem = RARITY_COLOR.get(rarity, ("#9AA3AD", "#C7CDD4"))
        ab_name, ab_line = _ability_line(flatt)

        champs.append(
            Champion(
                slug=slug,
                name=_grab(cmt, "display_name") or slug.title(),
                title=(_grab(mmt, "title") or "").strip(),
                faction=faction,
                archetype=_grab(cmt, "archetype") or "bruiser",
                rarity=rarity,
                cost=int(_grab_num(cmt, "elixir_cost") or 0),
                hp=int(float(_grab_num(cmt, "hitpoints") or 0)),
                ad=int(float(_grab_num(cmt, "attack_damage") or 0)),
                armor=int(float(_grab_num(cmt, "armor") or 0)),
                rng=float(_grab_num(cmt, "range") or 0),
                lore=_grab(mmt, "lore") or "",
                ability_name=ab_name,
                ability_line=ab_line,
                accent=accent,
                tint=tint,
                rarity_accent=r_accent,
                gem=gem,
            )
        )
    return champs


if __name__ == "__main__":
    cs = load_all()
    print(f"{len(cs)} champions")
    for c in cs[:3]:
        print("---", c.slug, c.faction, c.rarity, "cost", c.cost)
        print("  ability:", c.ability_name, "|", c.ability_line)
        print("  PROMPT:", c.prompt()[:160])
