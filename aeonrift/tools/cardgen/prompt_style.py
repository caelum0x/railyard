"""Prompt engineering for the Ascendancy card ILLUSTRATION pipeline.

The visual cohesion of AI art is 90% the prompt. We therefore build every
champion's prompt as:  SHARED_STYLE_SUFFIX  +  per-champion SUBJECT.

The shared suffix is the same for all 47 cards -> the illustrations look like one
art-directed set (same medium, lighting, framing, palette discipline) rather than
47 unrelated pictures. Only the subject clause changes, derived deterministically
from the champion's name / faction / archetype / lore.

Pure data + string building. No network calls live here.
"""

from __future__ import annotations

# The single style contract shared by every illustration in the game. Edit this
# ONE string to restyle the entire set.
SHARED_STYLE_SUFFIX = (
    "digital painting, painterly semi-realistic fantasy splash art, dramatic "
    "rim lighting, dark cinematic background, bold silhouette reading clearly at "
    "thumbnail size, centered heroic three-quarter portrait, ornate but clean, "
    "high detail on face and weapon, muted background so the subject pops, "
    "concept-art quality, trending game key-art, no text, no watermark, no border, "
    "no UI, single character, vertical composition"
)

# Per-faction art direction — colour + atmosphere cues that echo the procedural
# frame palette so illustration and frame agree.
_FACTION_ART = {
    "Greek":    "sun-bleached marble and gold, aegean blue sky, laurel motifs, classical heroic",
    "Norse":    "frost, steel-blue tones, snow and aurora, rugged norse mythology, runic carvings",
    "Roman":    "crimson and bronze, legionary armor, eagle standards, martial imperial grandeur",
    "Egyptian": "sand-gold and turquoise, hieroglyphs, desert dusk, divine egyptian regalia",
    "Ember":    "molten orange and ember sparks, volcanic glow, smouldering embers, heat haze",
    "Tide":     "deep cyan and teal, bioluminescent water, coral and pearl, oceanic depths",
    "Verdant":  "lush emerald green, vines and blossoms, dappled forest light, primal nature",
    "Umbral":   "violet and void-black, eldritch purple energy, starless dark, arcane corruption",
}

# Archetype -> pose / weapon cue so the silhouette matches the kit.
_ARCHE_ART = {
    "tank":     "heavily armored, massive shield, immovable defensive stance",
    "bruiser":  "battle-scarred warrior, heavy weapon mid-swing, aggressive stance",
    "mage":     "channeling arcane energy, glowing hands, flowing robes, spell aura",
    "marksman": "drawing a ranged weapon, poised and precise, mid-aim",
    "assassin": "agile and shadowed, dual blades, coiled to strike, low crouch",
    "support":  "radiant protective aura, staff or relic raised, guardian presence",
}


def _clean_lore(lore: str) -> str:
    """First sentence of lore, trimmed of the ALLCAPS archetype shout and DATA notes."""
    s = lore.split(".")[0].strip()
    # drop leading "Name — " and boilerplate
    if "—" in s:
        s = s.split("—", 1)[1].strip()
    return s[:160]


def build_prompt(model: dict) -> str:
    """Deterministic per-champion prompt = subject + shared style suffix."""
    fac = _FACTION_ART.get(model["faction"], "")
    arch = _ARCHE_ART.get(model["archetype"], "")
    lore = _clean_lore(model.get("lore", ""))
    subject_bits = [
        f'{model["name"]}',
        model["title"],
        f'a {model["archetype"]} of the {model["faction"]} pantheon',
        arch,
        lore,
        fac,
    ]
    subject = ", ".join(b for b in subject_bits if b)
    return f"{subject}. {SHARED_STYLE_SUFFIX}"


def negative_prompt() -> str:
    """Shared negative prompt to keep the set clean and on-model."""
    return ("text, letters, watermark, signature, frame, border, ui, hud, "
            "multiple characters, cropped head, extra limbs, deformed, blurry, "
            "low quality, cartoon, chibi, nsfw")
