# Korrathil

> Design notes for the `korrathil` champion bundle. Combat numbers live in the
> flat spec `content/champions/korrathil.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Umbral
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 5 elixir

## Lore

Korrathil — the VOIDGORGER, a diving bruiser. (Umbral faction) A ravenous thing that steps through a tear in the dark, Korrathil crashes onto prey, clamps them in place, and feeds until the wounds close on its own hide. Sticky, sustained, and hard to shake. Authored entirely as DSL data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 660 |
| Attack damage | 60 |
| Armor | 33 |
| Attack speed | 1.0 |
| Move speed | 11.5 |
| Range | 8.0 |

## Kit

### Q — Rift Charge

- Targeting: Skillshot, range 22.0
- Cooldown: 210, 55 cost
- Effects: dash 20; 60 damage (+60% AD)

### W — Devour

- Targeting: Target, range 9.0
- Cooldown: 150, 45 cost
- Effects: 58 damage (+70% AD); slow 60 ticks

### R — Gorge

- Targeting: SelfCast
- Cooldown: 260, 50 cost
- Effects: heal 90

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~283.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

