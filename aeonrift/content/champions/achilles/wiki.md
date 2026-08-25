# Achilles

> Design notes for the `achilles` champion bundle. Combat numbers live in the
> flat spec `content/champions/achilles.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Greek
- **Archetype:** bruiser
- **Ascendancy card:** Rare, 4 elixir

## Lore

Achilles — BRUISER. The near-invincible hero of Troy: hurls himself onto a foe with a stunning spear, cleaves everything around him, and shrugs off wounds in a burst of immortal rage.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 760 |
| Attack damage | 40 |
| Armor | 33 |
| Attack speed | 0.9 |
| Move speed | 10.0 |
| Range | 5.0 |

## Kit

### Q — Spear of Peleus

- Targeting: Target, range 13.0
- Cooldown: 85, 45 cost
- Effects: dash 13; 46 damage (+35% AD); stun 8 ticks

### W — Myrmidon Cleave

- Targeting: Area, range 7.0
- Cooldown: 45, 30 cost
- Effects: 44 damage (+35% AD)

### R — Immortal Rage

- Targeting: SelfCast
- Cooldown: 160, no cost
- Effects: heal 80

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Rare card is ~236.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

