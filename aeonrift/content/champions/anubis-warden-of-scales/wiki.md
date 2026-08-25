# Anubis, Warden Of Scales

> Design notes for the `anubis-warden-of-scales` champion bundle. Combat numbers live in the
> flat spec `content/champions/anubis-warden-of-scales.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Egyptian
- **Archetype:** mage
- **Ascendancy card:** Epic, 5 elixir

## Lore

Anubis — a TANK / CONTROLLER who locks enemies down for his team. High HP and armor, low damage. His Mummy's Grasp is a hard point-and-click stun, and the Weight of Judgment presses a whole area into a slow. Embalming Rite keeps him standing. All crowd control, all data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 760 |
| Attack damage | 41 |
| Armor | 38 |
| Attack speed | 0.8 |
| Move speed | 10.0 |
| Range | 5.0 |

## Kit

### Q — Mummy's Grasp

- Targeting: Target, range 12.0
- Cooldown: 90, 40 cost
- Effects: 50 damage (+30% AD); stun 11 ticks

### W — Weight of Judgment

- Targeting: Area, range 14.0
- Cooldown: 70, 50 cost
- Effects: 60 damage (+35% AD); slow 16 ticks

### R — Embalming Rite

- Targeting: SelfCast
- Cooldown: 170, no cost
- Effects: heal 100

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Epic card is ~257.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

