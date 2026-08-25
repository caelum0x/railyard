# Horus, Skyfall Talon

> Design notes for the `horus-skyfall-talon` champion bundle. Combat numbers live in the
> flat spec `content/champions/horus-skyfall-talon.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Egyptian
- **Archetype:** bruiser
- **Ascendancy card:** Rare, 4 elixir

## Lore

Horus — a BRUISER with a diving DASH engage and sustained melee pressure. Durable but hits harder than a pure tank. Falcon Dive closes and stuns; Talon Rend is a heavy, slowing melee strike; the War Cry sustains him in a brawl.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 720 |
| Attack damage | 31 |
| Armor | 32 |
| Attack speed | 0.9 |
| Move speed | 11.0 |
| Range | 5.0 |

## Kit

### Q — Falcon Dive

- Targeting: Target, range 16.0
- Cooldown: 85, 40 cost
- Effects: dash 16; 56 damage (+45% AD); stun 8 ticks

### W — Talon Rend

- Targeting: Target, range 6.0
- Cooldown: 40, 30 cost
- Effects: 44 damage (+40% AD); slow 10 ticks

### R — War Cry

- Targeting: SelfCast
- Cooldown: 160, no cost
- Effects: heal 110

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Rare card is ~227.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

