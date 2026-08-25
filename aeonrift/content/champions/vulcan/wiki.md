# Vulcan

> Design notes for the `vulcan` champion bundle. Combat numbers live in the
> flat spec `content/champions/vulcan.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Roman
- **Archetype:** tank
- **Ascendancy card:** Epic, 5 elixir

## Lore

Vulcan — the forge TANK / zoner. Durable frontline that controls space with molten ground.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 860 |
| Attack damage | 42 |
| Armor | 40 |
| Attack speed | 0.8 |
| Move speed | 9.5 |
| Range | 5.0 |

## Kit

### Q — Molten Anvil

- Targeting: Area, range 11.0
- Cooldown: 70, 50 cost
- Effects: 55 damage (+30% AD); slow 45 ticks

### W — Cinder Chains

- Targeting: Skillshot, range 20.0
- Cooldown: 110, 40 cost
- Effects: 40 damage (+20% AD); stun 10 ticks

### R — Forge Ward

- Targeting: SelfCast
- Cooldown: 150, no cost
- Effects: heal 130

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies tank growth
across levels 1-18. Deploy power for the Epic card is ~264.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

