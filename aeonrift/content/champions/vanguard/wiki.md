# Vanguard

> Design notes for the `vanguard` champion bundle. Combat numbers live in the
> flat spec `content/champions/vanguard.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Roman
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 5 elixir

## Lore

Vanguard — a BRUISER with a gap-closing DASH + STRIKE. Tanky and durable. Its signature ability dashes onto a target, strikes for AD-scaled damage, and briefly stuns — a classic engage. Demonstrates a multi-effect, targeted ability plus a self-heal, all as pure DSL data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 780 |
| Attack damage | 48 |
| Armor | 35 |
| Attack speed | 0.85 |
| Move speed | 10.0 |
| Range | 5.0 |

## Kit

### Q — Shield Charge

- Targeting: Target, range 14.0
- Cooldown: 90, 40 cost
- Effects: dash 14; 60 damage (+40% AD); stun 8 ticks

### W — Rally

- Targeting: SelfCast
- Cooldown: 180, no cost
- Effects: heal 120

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~274.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

