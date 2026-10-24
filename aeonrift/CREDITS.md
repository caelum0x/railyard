# Credits & AI-Asset Provenance

Aeonrift ships two kinds of generated content on top of original code. This file is
the single source of truth for what's AI-generated, what it was trained on, and what
it is **not** — required before any storefront listing (Steam, App Store, Google Play
all ask for an AI-content disclosure).

## Champion roster — original IP, hand-authored data

Every champion in `content/champions/*.ron` (47 total) is an original character:
name, lore, stats, and ability kit authored as data against the closed-vocabulary
ability DSL (`engine/ability-dsl`, see `docs/dsl-spec.md`). None are copies,
reskins, or stat-clones of any existing game's characters — "Greek Olympians",
"Norse Aesir", "Egyptian Pantheon", etc. are our own franchise's faction names
drawing on public-domain mythology (the same source material countless other
original games draw character concepts from), not any other product's roster.

## Card splash art — AI-generated, style-only LoRA

The 47 champion splash images (`assets/cards/final-v2/*.png`) are generated with a
custom-trained image model, not sourced, traced, or adapted from any existing artist's
work or any other game's assets:

- **Base model:** Flux-dev (Black Forest Labs), via Replicate.
- **Style LoRA:** trained in-house on 22 curated anchor images we commissioned/generated
  ourselves (`railyard/LORA.md`), specifically to lock one consistent "painterly
  action splash" look across the whole roster — not to imitate any named artist or
  any other studio's specific title.
- **Trigger word:** `AEONSPLASH`.
- **Caption discipline:** every training anchor used the *same* generic style caption
  (trigger token + lighting/composition descriptor only, no per-subject or per-artist
  captions), specifically so the LoRA learns a rendering style, not any individual
  character, artist, or copyrighted subject.
- **Model version:** `caelum0x/aeon-style-lora:433a51a0e3f751b24f29f4c93b140f2e484e91723abd4a26a80c7576bb3e4642`
  (full training recipe, hyperparameters, and trainer in `railyard/LORA.md`).
- **Human review:** every generated splash is reviewed against `tools/content-lint`
  and the roster QA pass before being committed; see `content/champions/README.md`
  for the per-card checklist.

**Storefront disclosure line (reusable verbatim):** "Character splash art for this
game was generated using a custom-trained AI image model (Flux-dev + an in-house
style LoRA). Character names, lore, stats, and gameplay are original and human-authored."

## Engine

- `engine/sim-core`, `engine/ability-dsl`, `games/aeonrift-ascendancy` (rules/cards/
  deck/modes/ai) — original Rust code written for this project, zero external deps
  on the deterministic path (see each crate's `Cargo.toml`).
- `engine/lol_*` / `engine/league_*` (the Bevy-based render/netcode layer) —
  migrated from the `moon-lol` fork; Riot WAD/PROP/TEX asset loaders are feature-gated
  **off by default** (`riot-assets`) and no Riot data or assets are copied into this
  repo. See the exclusion notes in the root `Cargo.toml` workspace comment and
  `MIGRATION.md`.
- Render fork: `bevy` via `github.com/mutemoon/bevy` (branch `main`) — third-party,
  MIT/Apache-2.0 dual-licensed upstream Bevy fork; not redistributed, pulled as a
  Cargo git dependency.

## What this is not

- Not a licensed/branded IP (no trademarked characters, logos, or copied lore).
- Not trained on, or captioned with, any other game's character names or any named
  real artist's name.
- Not using any voice, likeness, or performance of a real person.
