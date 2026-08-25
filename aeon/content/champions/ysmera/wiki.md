# Ysmera

> Design notes for the `ysmera` champion bundle. Combat numbers live in the
> flat spec `content/champions/ysmera.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Ember
- **Archetype:** mage
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Ysmera — the PYRE-ORACLE, a burst mage. Fragile but explosive: area detonations and a long-range lance that erase squishies in a single rotation. Low HP, high impact. Pure DATA.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 540 |
| Attack damage | 46 |
| Armor | 20 |
| Attack speed | 0.9 |
| Move speed | 11.0 |
| Range | 22.0 |

## Kit

### Q — Pyre Bloom

- Targeting: Area, range 20.0
- Cooldown: 180, 65 cost
- Effects: 120 damage (+70% AD)

### W — Ember Lance

- Targeting: Skillshot, range 30.0
- Cooldown: 120, 45 cost
- Effects: 95 damage (+60% AD); slow 75 ticks

### R — Kilnburst

- Targeting: Area, range 24.0
- Cooldown: 360, 100 cost
- Effects: 170 damage (+90% AD)

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Legendary card is ~395.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

