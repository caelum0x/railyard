"""Faction and rarity design tokens for the Ascendancy card visual system.

The whole point of this module is COHESION: every card in the game pulls its
colours, gradients and motif from ONE of eight faction palettes plus ONE of four
rarity treatments. Change a value here and the entire 47-card set restyles in
lock-step -- that is what makes the set read as a single, intentional game rather
than 47 unrelated pictures.

Pure data. No rendering, no I/O. Imported by svg_card.py and sigil.py.
"""

from __future__ import annotations

# --- Faction palettes -------------------------------------------------------
#
# Each faction defines a small, deliberate palette:
#   bg0/bg1   : the vertical background gradient (dark -> slightly lit)
#   frame0/1  : the metallic frame gradient (the "border")
#   primary   : the signature accent (ribbons, headings, stat icons)
#   light     : a brighter tint of primary (highlights, sheen)
#   sigil0/1  : the two-tone fill for the procedural art-slot emblem
#   art0/art1 : the radial background *behind* the sigil in the art slot
#   motif     : which emblem-drawing routine sigil.py uses
#   label     : human display name shown on the faction ribbon
#
# Colours are chosen so the eight factions are instantly distinguishable at
# thumbnail size while sharing the same value structure (dark ground, luminous
# accent) -- the trick real card games use for a cohesive line-up.

FACTIONS: dict[str, dict] = {
    "Greek": {
        "label": "GREEK PANTHEON",
        "bg0": "#1c1608", "bg1": "#0d0a04",
        "frame0": "#f2d580", "frame1": "#9c7b1e",
        "primary": "#e7c15a", "light": "#fff2c2",
        "sigil0": "#f4d878", "sigil1": "#b8892a",
        "art0": "#3a2f12", "art1": "#0f0c05",
        "motif": "greek",
    },
    "Norse": {
        "label": "NORSE AESIR",
        "bg0": "#0d1922", "bg1": "#050c12",
        "frame0": "#bfe0f2", "frame1": "#3f6d92",
        "primary": "#7fb4dd", "light": "#d6ecfb",
        "sigil0": "#a9d6f2", "sigil1": "#4c7ea8",
        "art0": "#16303f", "art1": "#06121a",
        "motif": "norse",
    },
    "Roman": {
        "label": "ROMAN LEGION",
        "bg0": "#24080b", "bg1": "#100305",
        "frame0": "#f0b48c", "frame1": "#8f2a2f",
        "primary": "#d64b46", "light": "#ffcf9e",
        "sigil0": "#e8a05c", "sigil1": "#a3352f",
        "art0": "#3c1013", "art1": "#150406",
        "motif": "roman",
    },
    "Egyptian": {
        "label": "EGYPTIAN NETERU",
        "bg0": "#1f1808", "bg1": "#0c0902",
        "frame0": "#e9c766", "frame1": "#a67a2a",
        "primary": "#e0b84a", "light": "#ffe9a8",
        "sigil0": "#ecc85e", "sigil1": "#3fb6a6",  # sand-gold + turquoise
        "art0": "#352a10", "art1": "#0e0b04",
        "motif": "egyptian",
    },
    "Ember": {
        "label": "EMBER COVENANT",
        "bg0": "#26100a", "bg1": "#0f0503",
        "frame0": "#ffb066", "frame1": "#b5410f",
        "primary": "#f2701c", "light": "#ffcf87",
        "sigil0": "#ff9a3c", "sigil1": "#d23c0c",
        "art0": "#43180a", "art1": "#160603",
        "motif": "ember",
    },
    "Tide": {
        "label": "TIDE DOMINION",
        "bg0": "#07222a", "bg1": "#030f14",
        "frame0": "#8ee6ee", "frame1": "#1a7f92",
        "primary": "#2bbccf", "light": "#bff3f8",
        "sigil0": "#57d6e4", "sigil1": "#1a8ea0",
        "art0": "#0c3a44", "art1": "#04151b",
        "motif": "tide",
    },
    "Verdant": {
        "label": "VERDANT WILD",
        "bg0": "#0f2410", "bg1": "#050f06",
        "frame0": "#a6e08a", "frame1": "#3d8a3a",
        "primary": "#5cb14f", "light": "#d0f0be",
        "sigil0": "#7fd06a", "sigil1": "#337d33",
        "art0": "#173a17", "art1": "#07160a",
        "motif": "verdant",
    },
    "Umbral": {
        "label": "UMBRAL VOID",
        "bg0": "#170b28", "bg1": "#080415",
        "frame0": "#c79bf2", "frame1": "#5e2f9c",
        "primary": "#9a5fe0", "light": "#e2caff",
        "sigil0": "#b483f0", "sigil1": "#6a34b4",
        "art0": "#291247", "art1": "#0d0620",
        "motif": "umbral",
    },
}

# Fallback used if a champion's faction is somehow unknown (keeps the pipeline
# from ever crashing on new content).
DEFAULT_FACTION = "Umbral"


def faction_tokens(name: str) -> dict:
    """Return the palette for a faction, falling back gracefully."""
    return FACTIONS.get(name, FACTIONS[DEFAULT_FACTION])


# --- Rarity treatments ------------------------------------------------------
#
# Rarity does NOT recolour the whole card (faction owns colour). Instead it
# layers a metal + gem + ornament treatment on top, exactly like premium card
# games: the faction tells you WHO, the rarity tells you HOW PRECIOUS.
#
#   gem       : the rarity gem colour (top-right medallion + accents)
#   gem_light : gem highlight
#   band      : outer accent hairline colour just inside the frame
#   frame_w   : frame border thickness (legendary is chunkiest)
#   ornaments : draw corner filigree flourishes?
#   glow      : draw an outer radiant glow (legendary showpiece)?
#   pips      : number of rank pips drawn under the rarity gem
#   label     : ribbon text

RARITIES: dict[str, dict] = {
    "Common": {
        "label": "COMMON",
        "gem": "#9aa4ad", "gem_light": "#e6ebef",
        "band": "#6b7580",
        "frame_w": 9, "ornaments": False, "glow": False, "pips": 1,
    },
    "Rare": {
        "label": "RARE",
        "gem": "#3fa9f5", "gem_light": "#bfe4ff",
        "band": "#2f7fc0",
        "frame_w": 11, "ornaments": False, "glow": False, "pips": 2,
    },
    "Epic": {
        "label": "EPIC",
        "gem": "#b061e8", "gem_light": "#e7c8ff",
        "band": "#7d38b8",
        "frame_w": 13, "ornaments": True, "glow": False, "pips": 3,
    },
    "Legendary": {
        "label": "LEGENDARY",
        "gem": "#f5c542", "gem_light": "#fff2b0",
        "band": "#d79a1f",
        "frame_w": 15, "ornaments": True, "glow": True, "pips": 4,
    },
}

DEFAULT_RARITY = "Common"


def rarity_tokens(name: str) -> dict:
    """Return the treatment for a rarity, falling back gracefully."""
    return RARITIES.get(name, RARITIES[DEFAULT_RARITY])
