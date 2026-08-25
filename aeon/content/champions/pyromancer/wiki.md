# Pyromancer

> Design notes for the `pyromancer` champion bundle. Combat numbers live in the
> flat spec `content/champions/pyromancer.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Ember
- **Archetype:** mage
- **Ascendancy card:** Rare, 4 elixir

## Lore

Pyromancer — a BURST MAGE built around a long-range SKILLSHOT nuke. Fragile in stats, but its Fireball out-ranges an auto-attacker and STUNS on hit, so it can poke and lock down a target before it ever closes the gap. Authored entirely as data against the closed effect vocabulary (Damage + Stun).

## Base stats

| Stat | Value |
| --- | --- |
| HP | 470 |
| Attack damage | 28 |
| Armor | 18 |
| Attack speed | 0.7 |
| Move speed | 9.0 |
| Range | 6.0 |

## Kit

### Q — Fireball

- Targeting: Skillshot, range 28.0
- Cooldown: 36, 60 cost
- Effects: 96 damage (+50% AD); stun 16 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Rare card is ~247.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

