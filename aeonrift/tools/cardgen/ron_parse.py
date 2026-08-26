"""Tiny, dependency-free reader for the small slice of RON the card renderer needs.

We deliberately do NOT implement a full RON parser. The Aeonrift content files
are hand-written and regular, and we only need a handful of scalar fields plus
the ability list. Regex extraction is robust enough here and keeps the whole
tool to the Python standard library (no `ron`, no `serde`, no build step).

Everything returned is a plain dict of primitives -- immutable inputs in, new
dict out, no mutation of caller state (see coding-style: immutability).
"""

from __future__ import annotations

import re
from pathlib import Path


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8") if path.exists() else ""


def _scalar(text: str, key: str) -> str | None:
    """Grab a `key: value,` scalar (string, number or ident)."""
    m = re.search(rf'{key}\s*:\s*"([^"]*)"', text)
    if m:
        return m.group(1)
    m = re.search(rf'{key}\s*:\s*([A-Za-z0-9_.\-]+)', text)
    return m.group(1) if m else None


def _num(text: str, key: str) -> float | None:
    v = _scalar(text, key)
    if v is None:
        return None
    try:
        return float(v)
    except ValueError:
        return None


def parse_champion_spec(text: str) -> dict:
    """Extract name, base stats and abilities from a ChampionSpec RON body."""
    name = _scalar(text, "name") or "Unknown"

    # base stats live inside BaseStats( ... ); slice it so ability numbers don't
    # bleed in.
    base_block = ""
    bm = re.search(r"BaseStats\((.*?)\)", text, re.DOTALL)
    if bm:
        base_block = bm.group(1)
    base = {
        "hp": _num(base_block, "hp"),
        "ad": _num(base_block, "ad"),
        "armor": _num(base_block, "armor"),
        "attack_speed": _num(base_block, "attack_speed"),
        "move_speed": _num(base_block, "move_speed"),
        "range": _num(base_block, "range"),
    }

    abilities = []
    for am in re.finditer(r"AbilitySpec\((.*?)\n\s*\),", text + "\n    ),", re.DOTALL):
        block = am.group(1)
        a_name = _scalar(block, "name")
        if not a_name:
            continue
        targeting = _scalar(block, "targeting")
        cost = _num(block, "cost")  # inside Some(..) -> _num finds the number
        cooldown = _num(block, "cooldown")
        rng = _num(block, "range")
        effects = _summarise_effects(block)
        abilities.append({
            "name": a_name,
            "targeting": targeting,
            "cost": cost,
            "cooldown": cooldown,
            "range": rng,
            "effects": effects,
        })

    return {"name": name, "base": base, "abilities": abilities}


# The Ascendancy effect vocabulary is small (Damage/Slow/Heal/Stun/Dash). We turn
# each effect ctor into a short, human-readable clause for the ability text box.
_EFFECT_ORDER = ["Dash", "Damage", "Stun", "Slow", "Heal", "Shield", "Root", "Silence"]


def _summarise_effects(block: str) -> list[str]:
    parts: list[str] = []
    seen: set[str] = set()

    # capture the numeric args we care about per effect kind
    dmg = re.search(r"Damage\(\s*amount:\s*([0-9]+)", block)
    ratio = re.search(r"AttackDamage\(ratio:\s*([0-9.]+)\)", block)
    stun = re.search(r"Stun\(\s*duration:\s*([0-9]+)", block)
    slow = re.search(r"Slow\(\s*duration:\s*([0-9]+)", block)
    heal = re.search(r"Heal\(\s*amount:\s*([0-9]+)", block)
    dash = re.search(r"Dash\(\s*distance:\s*([0-9.]+)", block)

    if dash:
        parts.append(f"Dash {int(float(dash.group(1)))}")
        seen.add("Dash")
    if dmg:
        clause = f"Deal {dmg.group(1)} dmg"
        if ratio:
            clause += f" (+{int(float(ratio.group(1)) * 100)}% AD)"
        parts.append(clause)
        seen.add("Damage")
    if stun:
        parts.append(f"Stun {stun.group(1)}")
        seen.add("Stun")
    if slow:
        parts.append(f"Slow {slow.group(1)}")
        seen.add("Slow")
    if heal:
        parts.append(f"Heal {heal.group(1)}")
        seen.add("Heal")

    return parts


def parse_meta(text: str) -> dict:
    """Extract narrative sidecar fields (title, faction, archetype, lore)."""
    return {
        "display_name": _scalar(text, "display_name"),
        "title": _scalar(text, "title") or "",
        "faction": _scalar(text, "faction"),
        "archetype": _scalar(text, "archetype"),
        "resource": _scalar(text, "resource"),
        "lore": _scalar(text, "lore") or "",
    }


def parse_card_meta(text: str) -> dict:
    """Extract Ascendancy card knobs: rarity, elixir cost, derived stats."""
    return {
        "rarity": _scalar(text, "rarity"),
        "elixir_cost": _num(text, "elixir_cost"),
        "archetype": _scalar(text, "archetype"),
        "faction": _scalar(text, "faction"),
        "deploy_power": _num(text, "deploy_power"),
        "hitpoints": _num(text, "hitpoints"),
        "attack_damage": _num(text, "attack_damage"),
        "armor": _num(text, "armor"),
        "range": _num(text, "range"),
        "approx_dps": _num(text, "approx_dps"),
    }


def parse_roster_meta(text: str) -> dict[str, dict]:
    """Parse roster_meta.ron into {slug: {archetype, faction}}."""
    out: dict[str, dict] = {}
    for m in re.finditer(
        r'"([a-z0-9\-]+)"\s*:\s*\(\s*archetype:\s*"([^"]+)"\s*,\s*faction:\s*"([^"]+)"',
        text,
    ):
        out[m.group(1)] = {"archetype": m.group(2), "faction": m.group(3)}
    return out


# --- High-level loader ------------------------------------------------------

def load_champion(slug: str, champ_dir: Path, roster: dict[str, dict]) -> dict:
    """Assemble the full card model for one champion from all its sidecars.

    Precedence for faction/archetype: card_meta -> meta -> roster_meta.
    Falls back sensibly so a missing sidecar never crashes the renderer.
    """
    spec = parse_champion_spec(_read(champ_dir / f"{slug}.ron"))
    meta = parse_meta(_read(champ_dir / slug / "meta.ron"))
    card = parse_card_meta(_read(champ_dir / slug / "card_meta.ron"))
    ros = roster.get(slug, {})

    faction = card.get("faction") or meta.get("faction") or ros.get("faction") or "Umbral"
    archetype = card.get("archetype") or meta.get("archetype") or ros.get("archetype") or "bruiser"

    # Elixir cost: from card_meta if present, else derive a sensible 1-10 band
    # from champion power (hp+ad heuristic) so the card is never blank.
    cost = card.get("elixir_cost")
    if cost is None:
        hp = spec["base"].get("hp") or 500
        ad = spec["base"].get("ad") or 40
        cost = max(1, min(10, round((hp / 130) + (ad / 25))))

    rarity = card.get("rarity") or _rarity_from_cost(int(cost))

    return {
        "slug": slug,
        "name": meta.get("display_name") or spec["name"],
        "title": _title_for(slug, meta, archetype),
        "faction": faction,
        "archetype": archetype,
        "rarity": rarity,
        "cost": int(cost),
        "base": spec["base"],
        "abilities": spec["abilities"],
        "stats": {
            "hp": card.get("hitpoints") or spec["base"].get("hp"),
            "ad": card.get("attack_damage") or spec["base"].get("ad"),
            "armor": card.get("armor") or spec["base"].get("armor"),
            "range": card.get("range") or spec["base"].get("range"),
            "dps": card.get("approx_dps"),
        },
        "lore": meta.get("lore", ""),
    }


def _rarity_from_cost(cost: int) -> str:
    if cost <= 3:
        return "Common"
    if cost <= 5:
        return "Rare"
    if cost <= 7:
        return "Epic"
    return "Legendary"


_ARCHE_TITLE = {
    "tank": "the Bulwark",
    "bruiser": "the Warbound",
    "mage": "the Arcanist",
    "marksman": "the Deadeye",
    "assassin": "the Shadowblade",
    "support": "the Warden",
}


def _title_for(slug: str, meta: dict, archetype: str) -> str:
    """Prefer the designer-set title; otherwise synthesise a clean archetype epithet."""
    t = (meta.get("title") or "").strip()
    if t:
        return t
    return _ARCHE_TITLE.get(archetype, "the Ascendant")
