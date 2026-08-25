# Zeus

> Design notes for the `zeus` champion bundle. Combat numbers live in the
> flat spec `content/champions/zeus.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Greek
- **Archetype:** mage
- **Ascendancy card:** Epic, 6 elixir

## Lore

Zeus — BURST MAGE. King of Olympus; a long-range lightning skillshot that stuns, backed by a storm that damages and slows everything around the aim point.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 610 |
| Attack damage | 38 |
| Armor | 22 |
| Attack speed | 0.7 |
| Move speed | 9.0 |
| Range | 6.0 |

## Kit

### Q — Thunderbolt

- Targeting: Skillshot, range 30.0
- Cooldown: 32, 65 cost
- Effects: 100 damage (+55% AD); stun 14 ticks

### W — Storm's Wrath

- Targeting: Area, range 12.0
- Cooldown: 120, 90 cost
- Effects: 130 damage (+60% AD); slow 30 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Epic card is ~331.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

