# Nyxralla

> Design notes for the `nyxralla` champion bundle. Combat numbers live in the
> flat spec `content/champions/nyxralla.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Tide
- **Archetype:** mage
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Nyxralla — a BURST MAGE. Fragile, long range, and detonates targets with a high-ratio lance and a slowing bloom. Everything here is DATA against the closed DSL.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 585 |
| Attack damage | 47 |
| Armor | 21 |
| Attack speed | 0.9 |
| Move speed | 11.0 |
| Range | 22.0 |

## Kit

### Q — Abyssal Lance

- Targeting: Skillshot, range 24.0
- Cooldown: 120, 70 cost
- Effects: 175 damage (+100% AD)

### W — Maelstrom Bloom

- Targeting: Area, range 20.0
- Cooldown: 240, 100 cost
- Effects: 210 damage (+80% AD); slow 60 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Legendary card is ~458.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

