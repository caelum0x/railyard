# Zeryth

> Design notes for the `zeryth` champion bundle. Combat numbers live in the
> flat spec `content/champions/zeryth.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Umbral
- **Archetype:** assassin
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Zeryth — the UMBRAL FANG, a burst assassin. (Umbral faction) A blade of living shadow that folds the distance to its mark, pins them mid- scream, and finishes in a collapse of darkness. High mobility and damage, little armor to spare. Pure DATA against the closed DSL.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 570 |
| Attack damage | 62 |
| Armor | 25 |
| Attack speed | 1.0 |
| Move speed | 12.0 |
| Range | 6.0 |

## Kit

### Q — Shadowlunge

- Targeting: Skillshot, range 20.0
- Cooldown: 170, 50 cost
- Effects: dash 20; 92 damage (+95% AD)

### W — Fangstrike

- Targeting: Target, range 8.0
- Cooldown: 150, 55 cost
- Effects: 82 damage (+85% AD); stun 28 ticks

### R — Umbral Collapse

- Targeting: Area, range 10.0
- Cooldown: 330, 90 cost
- Effects: 130 damage (+105% AD)

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies assassin growth
across levels 1-18. Deploy power for the Legendary card is ~374.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

