# Ascendancy Card Visual System (`tools/cardgen`)

A cohesive, license-safe **2D card look** for *Aeonrift: Ascendancy*, built the way
real card games are: **procedural vector for structure** (frame, rarity gem,
faction theming, stat/ability layout, typography) + a **key-gated AI hook** for the
unique illustration in the art slot.

Nothing here touches the Rust crates or the default build. It reads content and
writes files. **Fully headless** — it never opens a window.

```
tools/cardgen/
  factions.py             # 8 faction palettes + 4 rarity treatments (design tokens)
  ron_parse.py            # tiny stdlib-only reader for the champion .ron sidecars
  sigil.py                # procedural, deterministic faction EMBLEM for the art slot
  svg_card.py             # assembles one polished 600x840 card SVG
  render_cards.py         # driver: renders the whole set (SVG + PNG) + contact sheet
  prompt_style.py         # shared style suffix + per-champion subject prompt builder
  generate_illustrations.py  # KEY-GATED AI art hook (safe no-op without a key)
  make_board.py           # writes board_mockup.html (match-screen reference)
  board_mockup.html       # generated match UI mockup (open in a browser to view)
```

## Quick start

```bash
cd aeonrift
python3 tools/cardgen/render_cards.py        # 47 cards -> assets/cards/*.svg (+ *.png)
python3 tools/cardgen/make_board.py          # -> tools/cardgen/board_mockup.html
open assets/cards/index.html                 # contact sheet of the whole set (manual)
```

No third-party Python packages are needed for the procedural renderer — standard
library only.

## The design system

**Faction owns colour. Rarity owns the metal, gem and ornament.** That single rule
is what makes 47 cards read as one game.

- **8 factions**, each a deliberate palette (background gradient, metallic frame
  gradient, accent, two-tone sigil, art-slot radial) and a unique emblem motif:

  | Faction | Palette | Sigil motif |
  |---|---|---|
  | Greek | gold / marble | laurel rings + Greek-key + column |
  | Norse | steel-blue / frost | Valknut + runic rim |
  | Roman | crimson / bronze | eagle chevrons + laurel + standard |
  | Egyptian | sand-gold / turquoise | sun-disc + ankh + rays |
  | Ember | molten orange | radiating flame tongues + core |
  | Tide | cyan / teal | concentric waves + trident |
  | Verdant | emerald green | radial leaves + seed-of-life ring |
  | Umbral | violet / void | eldritch star + orbiting nodes |

- **4 rarities** layer a treatment on top of the faction frame: gem colour +
  rank pips, outer accent hairline, border thickness, corner filigree (Epic /
  Legendary), and an outer radiant **glow** (Legendary only).

  | Rarity | Gem | Border | Ornaments | Glow |
  |---|---|---|---|---|
  | Common | pewter | thin | — | — |
  | Rare | blue | — | — | — |
  | Epic | violet | thick | filigree | — |
  | Legendary | gold | thickest | filigree | yes |

**Card layout (600×840, 5:7):** cost badge (top-left) · name + title (centered
serif) · rarity gem + pips (top-right) · framed **art slot** with sigil or AI art ·
faction/archetype ribbon · four stat pills (HP / AD / ARM / RNG, iconed) · abilities
panel (name + effect summary + targeting chip, faction sigil watermark behind) ·
footer (`AEONRIFT · ASCENDANCY` + rarity). All data — stats, abilities, cost, rarity,
faction — is pulled live from `content/champions/**`, so cards track balance changes.

## SVG vs PNG — rasterisation reality

The renderer always emits **SVG** (self-contained, resolution-independent,
license-safe). It *also* emits **PNG** when a cheap rasteriser is on `PATH`, tried
in this order:

1. `rsvg-convert` (librsvg) — **present on this machine (v2.61)**, used for the
   committed PNGs.
2. `cairosvg` (CLI or Python module) — fallback.
3. none found → **SVG-only**, with a printed note. Install one of:
   `brew install librsvg` · `pip install cairosvg`.

Run `python3 tools/cardgen/render_cards.py --no-png` to force SVG-only.

## AI illustration hook (Step 2) — where it plugs in

The art slot uses the procedural faction **sigil** by default. If a file
`assets/cards/art/<slug>.png` exists, `svg_card.py` drops it into the slot instead
(`<image ... preserveAspectRatio="xMidYMid slice">`, clipped to the framed
region). That is the entire integration surface — **provide the PNG and re-render.**

`generate_illustrations.py` produces those PNGs. It is **safe by default**: with no
key it is a dry run that prints the prompt it *would* send and writes nothing.

```bash
# 1) Dry run — see the prompts, call nothing (default):
python3 tools/cardgen/generate_illustrations.py

# 2) Real run — you supply the provider + your own key:
export IMAGE_API=replicate        # or: openai | stability
export IMAGE_API_KEY=****          # never hardcoded; read from env only
python3 tools/cardgen/generate_illustrations.py --go

# 3) Composite the new art into the cards:
python3 tools/cardgen/render_cards.py
```

**Prompt strategy (why it stays cohesive):** every prompt is
`per-champion subject` + one **shared style suffix** (`prompt_style.SHARED_STYLE_SUFFIX`).
The subject is derived deterministically from name / title / faction / archetype /
lore; the suffix (medium, lighting, framing, "no text/border/UI") is identical for
all 47 cards, so the illustrations look art-directed as a set. Edit that one string
to restyle the whole game.

**Cost:** roughly **US$0.003–0.04 per image** depending on provider/model
(Flux-schnell cheapest, `gpt-image-1` / Stability Core pricier), so one full 47-card
pass is about **US$0.15–2.00**. Always dry-run first. The hook never invents a key
and never calls a live API without `--go` + both env vars set.

## Board mockup (Step 3)

`make_board.py` writes `board_mockup.html` — a self-contained (all CSS + SVG inlined,
zero network) reference of the match screen: enemy/your base + tower HP, two lanes
with a river, deployed unit tokens (faction-tinted discs + HP bars), an elixir bar,
and a **hand of four real generated cards**. Open it in a browser to view; the
generator only writes the file.
