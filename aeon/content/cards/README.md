# content/cards

Card-only data for **Aeonrift: Ascendancy** (the mobile card-strategy game). The
combat data lives in `content/champions/*.ron`; everything here is the layer that turns
those champions into collectible cards. Loaded by `games/aeonrift-ascendancy`
(`cards::cards` + `cards::deck`).

- `rarities.ron` — rarity tiers (cost bands + pack draw weights).
- `tuning.ron` — card-only economy tuning (elixir power divisor + cost clamp).
- `packs/*.ron` — pack definitions (card count + guaranteed minimum rarity).
- `decks/*.ron` — the 12 preset decks (8 champion slugs each, validated against the roster).

All `.ron` here is CI-gated by `tools/content-lint` against `_schema`.
