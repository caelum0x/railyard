# Bellona

> Design notes for the `bellona` champion bundle. Combat numbers live in the
> flat spec `content/champions/bellona.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Roman
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 5 elixir

## Lore

Bellona — the weapon-master MARKSMAN. Fast, long-range auto-attacks plus a pinning volley.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 590 |
| Attack damage | 60 |
| Armor | 24 |
| Attack speed | 1.15 |
| Move speed | 11.0 |
| Range | 9.0 |

## Kit

### Q — Shattering Volley

- Targeting: Skillshot, range 22.0
- Cooldown: 40, 35 cost
- Effects: 70 damage (+60% AD); slow 30 ticks

### W — Blade Dance

- Targeting: Area, range 7.0
- Cooldown: 90, 40 cost
- Effects: 60 damage (+50% AD)

### R — Master's Guard

- Targeting: SelfCast
- Cooldown: 140, no cost
- Effects: heal 75

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~289.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

