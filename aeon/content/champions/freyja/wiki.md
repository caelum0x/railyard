# Freyja

> Design notes for the `freyja` champion bundle. Combat numbers live in the
> flat spec `content/champions/freyja.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Norse
- **Archetype:** mage
- **Ascendancy card:** Epic, 5 elixir

## Lore

Freyja — an enchanter SUPPORT: mend, snare, and smother the enemy line. Lady of the Vanir and the Brisingamen. Not a duelist: her value is a big self-sustain heal, a snaring skillshot that stuns, and a falcon-cloak area that slows. Peel and control expressed entirely as closed-vocabulary data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 620 |
| Attack damage | 48 |
| Armor | 26 |
| Attack speed | 0.85 |
| Move speed | 10.5 |
| Range | 7.0 |

## Kit

### Q — Blessing of Folkvangr

- Targeting: SelfCast
- Cooldown: 130, 60 cost
- Effects: heal 140

### W — Brisingamen Snare

- Targeting: Skillshot, range 22.0
- Cooldown: 50, 50 cost
- Effects: 70 damage (+40% AD); stun 12 ticks

### R — Falcon Cloak

- Targeting: Area, range 12.0
- Cooldown: 70, 45 cost
- Effects: 75 damage (+35% AD); slow 30 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Epic card is ~275.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

