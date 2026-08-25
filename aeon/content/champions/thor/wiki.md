# Thor

> Design notes for the `thor` champion bundle. Combat numbers live in the
> flat spec `content/champions/thor.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Norse
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 5 elixir

## Lore

Thor — a storm BRUISER: throw the hammer, charge in, call the thunder. Durable and mobile with real burst. Mjölnir is a long skillshot that stuns; Thunder Charge is a gap-closing dash-strike; God of Thunder is an area shock that slows. Multi-effect abilities, all authored as closed-vocabulary data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 760 |
| Attack damage | 40 |
| Armor | 34 |
| Attack speed | 0.9 |
| Move speed | 10.5 |
| Range | 5.0 |

## Kit

### Q — Mjolnir Hurl

- Targeting: Skillshot, range 24.0
- Cooldown: 60, 50 cost
- Effects: 66 damage (+40% AD); stun 12 ticks

### W — Thunder Charge

- Targeting: Target, range 14.0
- Cooldown: 80, 45 cost
- Effects: dash 14; 60 damage (+40% AD)

### R — God of Thunder

- Targeting: Area, range 12.0
- Cooldown: 100, 60 cost
- Effects: 70 damage (+40% AD); slow 15 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~269.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

