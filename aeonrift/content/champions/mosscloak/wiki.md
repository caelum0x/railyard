# Mosscloak

> Design notes for the `mosscloak` champion bundle. Combat numbers live in the
> flat spec `content/champions/mosscloak.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Verdant
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 5 elixir

## Lore

Mosscloak — a frontline TANK. Soaks damage, roots a single threat, and regrows his own bark. Built to hold a line, not to close a kill. Pure DSL data over the closed vocabulary.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 690 |
| Attack damage | 48 |
| Armor | 38 |
| Attack speed | 0.9 |
| Move speed | 10.5 |
| Range | 8.0 |

## Kit

### Q — Root Snare

- Targeting: Target, range 9.0
- Cooldown: 240, no cost
- Effects: stun 45 ticks; 40 damage (+30% AD)

### W — Barkskin

- Targeting: SelfCast
- Cooldown: 180, no cost
- Effects: heal 110

### R — Thorn Coil

- Targeting: Area, range 9.0
- Cooldown: 210, no cost
- Effects: 55 damage (+40% AD); slow 60 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~256.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

