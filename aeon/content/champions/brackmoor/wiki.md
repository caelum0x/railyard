# Brackmoor

> Design notes for the `brackmoor` champion bundle. Combat numbers live in the
> flat spec `content/champions/brackmoor.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Tide
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 5 elixir

## Lore

Brackmoor — a frontline TANK. Beefy frame, a hard point-and-click freeze, self-sustain, and a slowing shockwave to peel for allies. Pure DATA against the closed DSL.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 640 |
| Attack damage | 52 |
| Armor | 36 |
| Attack speed | 0.85 |
| Move speed | 10.0 |
| Range | 5.0 |

## Kit

### Q — Glacial Anchor

- Targeting: Target, range 6.0
- Cooldown: 240, 60 cost
- Effects: stun 45 ticks; 70 damage (+40% AD)

### W — Rimeguard

- Targeting: SelfCast
- Cooldown: 180, 50 cost
- Effects: heal 85

### R — Permafrost Wake

- Targeting: Area, range 9.0
- Cooldown: 210, 55 cost
- Effects: 60 damage (+30% AD); slow 90 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~279.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

