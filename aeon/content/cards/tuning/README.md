# content/cards/tuning

Card-only tuning knobs for Aeonrift: Ascendancy. These files govern the card game
mode's economy and pacing and are deliberately kept **separate** from the MOBA
combat numbers in `content/champions/*.ron`.

| File | Purpose |
|------|---------|
| `elixir_curve.ron`     | Elixir pool, regeneration, cost brackets, deck-average guardrail |
| `deploy_cooldowns.ron` | Per-rarity redeploy cooldowns with per-archetype multipliers |
| `rarity_pity.ron`      | Soft/hard pity timers for pack opens |
| `upgrade_curve.ron`    | Card leveling: copy + shard costs, per-level stat scalar |
| `pack_economy.ron`     | Currencies, duplicate overflow, earn caps |

Rarity ids referenced here must exist in `../rarities.ron`; archetype ids match
`../../champions/roster_meta.ron`.
