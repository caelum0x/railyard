# Neptune

> Design notes for the `neptune` champion bundle. Combat numbers live in the
> flat spec `content/champions/neptune.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Roman
- **Archetype:** mage
- **Ascendancy card:** Epic, 5 elixir

## Lore

Neptune — the tide BURST MAGE. Fragile, long-range wave burst with heavy slows.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 560 |
| Attack damage | 36 |
| Armor | 22 |
| Attack speed | 0.7 |
| Move speed | 9.0 |
| Range | 6.0 |

## Kit

### Q — Tidal Surge

- Targeting: Skillshot, range 26.0
- Cooldown: 34, 60 cost
- Effects: 100 damage (+50% AD); slow 40 ticks

### W — Maelstrom

- Targeting: Area, range 9.0
- Cooldown: 90, 70 cost
- Effects: 85 damage (+40% AD); slow 45 ticks

### R — Trident's Wrath

- Targeting: Target, range 12.0
- Cooldown: 70, 55 cost
- Effects: 70 damage (+30% AD); stun 12 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Epic card is ~279.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

