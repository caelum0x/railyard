# Tyr

> Design notes for the `tyr` champion bundle. Combat numbers live in the
> flat spec `content/champions/tyr.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Norse
- **Archetype:** tank
- **Ascendancy card:** Epic, 5 elixir

## Lore

Tyr — a stalwart TANK who trades blows so allies don't have to. The one-handed god of law and sacrifice: he binds a single foe in place and endures. Massive HP/armor, low damage, a hard single-target lockdown, an area cripple, and a heavy self-sustain — all pure DSL data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 880 |
| Attack damage | 44 |
| Armor | 44 |
| Attack speed | 0.8 |
| Move speed | 10.0 |
| Range | 5.0 |

## Kit

### Q — Oath of the Bound

- Targeting: Target, range 12.0
- Cooldown: 120, 50 cost
- Effects: 50 damage (+30% AD); stun 24 ticks

### W — Warden's Toll

- Targeting: Area, range 10.0
- Cooldown: 90, 40 cost
- Effects: 40 damage; slow 30 ticks

### R — Unyielding Sacrifice

- Targeting: SelfCast
- Cooldown: 210, no cost
- Effects: heal 160

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies tank growth
across levels 1-18. Deploy power for the Epic card is ~264.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

