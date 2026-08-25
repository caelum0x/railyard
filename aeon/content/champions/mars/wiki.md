# Mars

> Design notes for the `mars` champion bundle. Combat numbers live in the
> flat spec `content/champions/mars.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Roman
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 5 elixir

## Lore

Mars — the war BRUISER. Distinct from Ares: disciplined Roman general, not raw fury.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 760 |
| Attack damage | 46 |
| Armor | 33 |
| Attack speed | 0.9 |
| Move speed | 10.5 |
| Range | 5.0 |

## Kit

### Q — Spear of Conquest

- Targeting: Target, range 13.0
- Cooldown: 80, 45 cost
- Effects: dash 13; 56 damage (+40% AD); stun 9 ticks

### W — Crimson March

- Targeting: Area, range 10.0
- Cooldown: 100, 50 cost
- Effects: 50 damage (+30% AD); slow 30 ticks

### R — Battle Fury

- Targeting: SelfCast
- Cooldown: 160, no cost
- Effects: heal 100

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~262.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

