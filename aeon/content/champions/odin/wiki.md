# Odin

> Design notes for the `odin` champion bundle. Combat numbers live in the
> flat spec `content/champions/odin.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Norse
- **Archetype:** mage
- **Ascendancy card:** Epic, 5 elixir

## Lore

Odin — a BURST MAGE and battlefield commander wielding Gungnir. Fragile but with the longest reach on the roster: the spear that never misses bursts and stuns from afar, his ravens carve an area and slow, and Valhalla's Call restores him. Low HP, high skillshot damage — pure DSL data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 500 |
| Attack damage | 30 |
| Armor | 20 |
| Attack speed | 0.75 |
| Move speed | 9.5 |
| Range | 7.0 |

## Kit

### Q — Gungnir's Flight

- Targeting: Skillshot, range 30.0
- Cooldown: 40, 65 cost
- Effects: 100 damage (+60% AD); stun 14 ticks

### W — Huginn and Muninn

- Targeting: Area, range 14.0
- Cooldown: 70, 70 cost
- Effects: 85 damage (+50% AD); slow 18 ticks

### R — Valhalla's Call

- Targeting: SelfCast
- Cooldown: 150, no cost
- Effects: heal 90

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Epic card is ~260.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

