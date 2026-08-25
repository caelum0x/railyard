# Xelphine

> Design notes for the `xelphine` champion bundle. Combat numbers live in the
> flat spec `content/champions/xelphine.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Umbral
- **Archetype:** mage
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Xelphine — the ECLIPSE SEER, a burst mage. (Umbral faction) She reads the moment the light dies and pours that darkness into a single devastating rotation. Fragile and long-ranged, she blooms shadow at her feet, lances distant prey, and calls a total nightfall to erase what remains. DATA.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 555 |
| Attack damage | 47 |
| Armor | 21 |
| Attack speed | 0.9 |
| Move speed | 11.0 |
| Range | 22.0 |

## Kit

### Q — Eclipse Bloom

- Targeting: Area, range 20.0
- Cooldown: 180, 65 cost
- Effects: 120 damage (+70% AD)

### W — Umbral Lance

- Targeting: Skillshot, range 28.0
- Cooldown: 120, 45 cost
- Effects: 95 damage (+60% AD); slow 75 ticks

### R — Nightfall

- Targeting: Area, range 24.0
- Cooldown: 360, 100 cost
- Effects: 170 damage (+90% AD)

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Legendary card is ~399.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

