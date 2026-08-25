# Nerreth

> Design notes for the `nerreth` champion bundle. Combat numbers live in the
> flat spec `content/champions/nerreth.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Ember
- **Archetype:** bruiser
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Nerreth — the ASHSTALKER, a burst assassin. Blinks onto a target, dumps a stun-and-burst combo, and disengages. High mobility and damage, low durability. Pure DATA.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 560 |
| Attack damage | 58 |
| Armor | 22 |
| Attack speed | 1.0 |
| Move speed | 12.0 |
| Range | 8.0 |

## Kit

### Q — Haze Blink

- Targeting: Skillshot, range 22.0
- Cooldown: 180, 50 cost
- Effects: dash 22

### W — Emberpierce

- Targeting: Target, range 9.0
- Cooldown: 150, 55 cost
- Effects: 95 damage (+90% AD); stun 30 ticks

### R — Cremate

- Targeting: Area, range 10.0
- Cooldown: 330, 90 cost
- Effects: 150 damage (+110% AD)

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Legendary card is ~393.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

