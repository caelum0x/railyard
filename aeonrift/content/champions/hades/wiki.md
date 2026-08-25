# Hades

> Design notes for the `hades` champion bundle. Combat numbers live in the
> flat spec `content/champions/hades.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Greek
- **Archetype:** tank
- **Ascendancy card:** Epic, 5 elixir

## Lore

Hades — TANK / void controller. Lord of the Underworld: durable, drags a foe in with a stun, blankets the ground in soul-fire that damages and slows, and feeds on the dead to sustain.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 830 |
| Attack damage | 41 |
| Armor | 39 |
| Attack speed | 0.8 |
| Move speed | 9.5 |
| Range | 5.0 |

## Kit

### Q — Grasp of the Underworld

- Targeting: Target, range 11.0
- Cooldown: 100, 50 cost
- Effects: 55 damage (+35% AD); stun 8 ticks

### W — Field of Judgment

- Targeting: Area, range 11.0
- Cooldown: 130, 70 cost
- Effects: 80 damage (+30% AD); slow 40 ticks

### R — Soul Harvest

- Targeting: SelfCast
- Cooldown: 170, no cost
- Effects: heal 130

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies tank growth
across levels 1-18. Deploy power for the Epic card is ~294.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

