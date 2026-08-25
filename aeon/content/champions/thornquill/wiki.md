# Thornquill

> Design notes for the `thornquill` champion bundle. Combat numbers live in the
> flat spec `content/champions/thornquill.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Verdant
- **Archetype:** marksman
- **Ascendancy card:** Epic, 5 elixir

## Lore

Thornquill — a MARKSMAN. Long-range, high attack speed, low defenses. Rains scaling barbs, cripples with a piercing quill, and springs away to keep distance. DSL data only.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 540 |
| Attack damage | 60 |
| Armor | 20 |
| Attack speed | 1.15 |
| Move speed | 11.5 |
| Range | 16.0 |

## Kit

### Q — Barbed Volley

- Targeting: Skillshot, range 17.0
- Cooldown: 90, 50 cost
- Effects: 55 damage (+100% AD)

### W — Piercing Quill

- Targeting: Target, range 15.0
- Cooldown: 120, 45 cost
- Effects: 40 damage (+60% AD); slow 60 ticks

### R — Rootspring

- Targeting: SelfCast
- Cooldown: 180, no cost
- Effects: dash 10

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies marksman growth
across levels 1-18. Deploy power for the Epic card is ~262.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

