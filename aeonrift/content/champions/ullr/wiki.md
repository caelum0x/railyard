# Ullr

> Design notes for the `ullr` champion bundle. Combat numbers live in the
> flat spec `content/champions/ullr.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Norse
- **Archetype:** marksman
- **Ascendancy card:** Epic, 5 elixir

## Lore

Ullr — a MARKSMAN: the god of the bow, the hunt, and the winter frost. Long auto-attack range and high attack speed are his bread and butter; his abilities pile on a frost skillshot that slows, a raining volley over an area, and a bone-skate reposition dash. Sustained ranged damage as pure DSL data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 540 |
| Attack damage | 60 |
| Armor | 22 |
| Attack speed | 1.3 |
| Move speed | 10.5 |
| Range | 11.0 |

## Kit

### Q — Frostpiercer

- Targeting: Skillshot, range 26.0
- Cooldown: 40, 40 cost
- Effects: 70 damage (+60% AD); slow 20 ticks

### W — Volley of the Hunt

- Targeting: Area, range 18.0
- Cooldown: 60, 50 cost
- Effects: 80 damage (+70% AD)

### R — Bone Skates

- Targeting: Skillshot, range 12.0
- Cooldown: 100, 30 cost
- Effects: dash 12

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies marksman growth
across levels 1-18. Deploy power for the Epic card is ~297.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

