# Athena

> Design notes for the `athena` champion bundle. Combat numbers live in the
> flat spec `content/champions/athena.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Greek
- **Archetype:** support
- **Ascendancy card:** Epic, 5 elixir

## Lore

Athena — SUPPORT / guardian. Goddess of wisdom and strategic war: raises the Aegis to shield herself, snares foes with a slowing owl, and commands a phalanx that stuns and slows a cluster of enemies.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 680 |
| Attack damage | 50 |
| Armor | 33 |
| Attack speed | 0.85 |
| Move speed | 10.0 |
| Range | 6.0 |

## Kit

### Q — Aegis of Wisdom

- Targeting: SelfCast
- Cooldown: 140, 60 cost
- Effects: heal 115

### W — Owl's Snare

- Targeting: Skillshot, range 20.0
- Cooldown: 80, 45 cost
- Effects: 75 damage (+40% AD); slow 35 ticks

### R — Phalanx Command

- Targeting: Area, range 12.0
- Cooldown: 110, 55 cost
- Effects: 70 damage (+40% AD); stun 10 ticks; slow 20 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies support growth
across levels 1-18. Deploy power for the Epic card is ~287.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

