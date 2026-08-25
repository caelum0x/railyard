# Ra, Radiant Dawn

> Design notes for the `ra-radiant-dawn` champion bundle. Combat numbers live in the
> flat spec `content/champions/ra-radiant-dawn.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Egyptian
- **Archetype:** mage
- **Ascendancy card:** Epic, 5 elixir

## Lore

Ra — a BURST MAGE built around area SUN damage. Fragile like every mage, but Ra deletes clustered enemies with a wide Sunfire Nova and pokes from absurd range with a piercing Solar Lance. Each dawn he is reborn (a small self-heal). Authored entirely as data against the closed vocab.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 480 |
| Attack damage | 30 |
| Armor | 18 |
| Attack speed | 0.75 |
| Move speed | 9.0 |
| Range | 7.0 |

## Kit

### Q — Sunfire Nova

- Targeting: Area, range 20.0
- Cooldown: 40, 70 cost
- Effects: 104 damage (+45% AD); slow 10 ticks

### W — Solar Lance

- Targeting: Skillshot, range 28.0
- Cooldown: 30, 45 cost
- Effects: 68 damage (+35% AD)

### R — Reborn at Dawn

- Targeting: SelfCast
- Cooldown: 200, no cost
- Effects: heal 100

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Epic card is ~263.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

