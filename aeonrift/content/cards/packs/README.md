# content/cards/packs

Pack definitions for Aeonrift: Ascendancy's card mode. Each `*.ron` file is one
`CardPack` describing its name, cost, card count, and `rarity_odds`.

- Every `rarity` id referenced here must exist in `../rarities.ron`.
- `weight` values are relative pull weights within a single open (they do not need to
  sum to any fixed total).
- `guaranteed_floor: Some("<rarity>")` forces at least one card of that rarity or
  better; it composes with the pity system in `../tuning/rarity_pity.ron`.
- `cost.currency` must be one of the currencies in `../tuning/pack_economy.ron`.
- `faction_bias` / `pool_bias` (optional) weight the card pool toward a faction from
  `../../champions/roster_meta.ron` or an archetype.

These are additive content files only — they never modify the flat
`content/champions/*.ron` MOBA specs.
