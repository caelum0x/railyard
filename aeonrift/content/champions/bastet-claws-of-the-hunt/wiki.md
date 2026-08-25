# Bastet, Claws Of The Hunt

> Design notes for the `bastet-claws-of-the-hunt` champion bundle. Combat numbers live in the
> flat spec `content/champions/bastet-claws-of-the-hunt.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Egyptian
- **Archetype:** assassin
- **Ascendancy card:** Rare, 4 elixir

## Lore

Bastet — a MARKSMAN / auto-attacker: high attack speed, long range, feline mobility. Damage comes from fast basics; her kit sets them up. Pounce repositions with a short dash, Rending Claws hits hard and slows so she can kite, and Feral Frenzy (nine lives) restores her when cornered.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 560 |
| Attack damage | 41 |
| Armor | 20 |
| Attack speed | 0.85 |
| Move speed | 11.0 |
| Range | 9.0 |

## Kit

### Q — Pounce

- Targeting: Target, range 12.0
- Cooldown: 60, 30 cost
- Effects: dash 12; 60 damage (+50% AD)

### W — Rending Claws

- Targeting: Target, range 9.0
- Cooldown: 30, 25 cost
- Effects: 56 damage (+55% AD); slow 8 ticks

### R — Feral Frenzy

- Targeting: SelfCast
- Cooldown: 150, no cost
- Effects: heal 90

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies assassin growth
across levels 1-18. Deploy power for the Rare card is ~233.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

