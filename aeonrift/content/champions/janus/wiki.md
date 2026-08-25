# Janus

> Design notes for the `janus` champion bundle. Combat numbers live in the
> flat spec `content/champions/janus.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Roman
- **Archetype:** mage
- **Ascendancy card:** Epic, 6 elixir

## Lore

Janus — the two-faced portal SUPPORT. Mobility, sustain, and a gate-slam lockdown.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 760 |
| Attack damage | 58 |
| Armor | 36 |
| Attack speed | 1.1 |
| Move speed | 10.0 |
| Range | 6.0 |

## Kit

### Q — Closing Door

- Targeting: Skillshot, range 20.0
- Cooldown: 45, 50 cost
- Effects: 75 damage (+40% AD); slow 30 ticks; stun 10 ticks

### W — Gate of Passage

- Targeting: SelfCast
- Cooldown: 60, 30 cost
- Effects: dash 15

### R — Threshold Ward

- Targeting: SelfCast
- Cooldown: 130, no cost
- Effects: heal 120

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Epic card is ~312.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

