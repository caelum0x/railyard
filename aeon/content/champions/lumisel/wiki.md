# Lumisel

> Design notes for the `lumisel` champion bundle. Combat numbers live in the
> flat spec `content/champions/lumisel.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Tide
- **Archetype:** support
- **Ascendancy card:** Rare, 4 elixir

## Lore

Lumisel — an enchanter SUPPORT. Two heals of different weights plus a slowing, chip-damage zone for peel. Low personal damage by design. Authored as DATA against the closed DSL.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 600 |
| Attack damage | 44 |
| Armor | 26 |
| Attack speed | 0.9 |
| Move speed | 11.0 |
| Range | 18.0 |

## Kit

### Q — Tidal Blessing

- Targeting: SelfCast
- Cooldown: 120, 60 cost
- Effects: heal 105

### W — Frostwater Veil

- Targeting: Area, range 11.0
- Cooldown: 180, 70 cost
- Effects: slow 90 ticks; 40 damage

### R — Riptide Ward

- Targeting: SelfCast
- Cooldown: 90, 40 cost
- Effects: heal 50

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies support growth
across levels 1-18. Deploy power for the Rare card is ~216.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

