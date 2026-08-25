# Sporeveil

> Design notes for the `sporeveil` champion bundle. Combat numbers live in the
> flat spec `content/champions/sporeveil.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Verdant
- **Archetype:** mage
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Sporeveil — a BURST MAGE. Fragile, ranged, and explosive: detonate a spore bloom for heavy AoE burst, smother a zone in pollen, and snipe stragglers. Pure DSL data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 580 |
| Attack damage | 48 |
| Armor | 24 |
| Attack speed | 0.85 |
| Move speed | 11.0 |
| Range | 20.0 |

## Kit

### Q — Spore Bloom

- Targeting: Area, range 20.0
- Cooldown: 210, 80 cost
- Effects: 175 damage (+60% AD)

### W — Choking Pollen

- Targeting: Area, range 18.0
- Cooldown: 150, 60 cost
- Effects: 55 damage; slow 75 ticks

### R — Fungal Lance

- Targeting: Skillshot, range 24.0
- Cooldown: 90, 40 cost
- Effects: 90 damage (+70% AD)

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Legendary card is ~411.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

