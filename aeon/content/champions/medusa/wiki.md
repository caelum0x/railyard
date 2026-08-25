# Medusa

> Design notes for the `medusa` champion bundle. Combat numbers live in the
> flat spec `content/champions/medusa.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Greek
- **Archetype:** marksman
- **Ascendancy card:** Epic, 5 elixir

## Lore

Medusa — MARKSMAN (petrify). Fast, long-range auto-attacker whose gaze turns a target to stone (a long stun), backed by a venom shot that slows on impact.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 600 |
| Attack damage | 62 |
| Armor | 24 |
| Attack speed | 1.15 |
| Move speed | 9.5 |
| Range | 9.0 |

## Kit

### Q — Petrifying Gaze

- Targeting: Skillshot, range 22.0
- Cooldown: 60, 70 cost
- Effects: 70 damage (+50% AD); stun 16 ticks

### W — Venom Volley

- Targeting: Skillshot, range 16.0
- Cooldown: 40, 40 cost
- Effects: 60 damage (+60% AD); slow 24 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies marksman growth
across levels 1-18. Deploy power for the Epic card is ~294.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

