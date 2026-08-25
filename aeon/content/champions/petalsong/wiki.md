# Petalsong

> Design notes for the `petalsong` champion bundle. Combat numbers live in the
> flat spec `content/champions/petalsong.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Verdant
- **Archetype:** support
- **Ascendancy card:** Rare, 4 elixir

## Lore

Petalsong — a disruptor SUPPORT. Locks down a key target, smothers a group with clinging vines, and sings her own wounds shut. Peels and sustains rather than bursts. Pure DSL data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 620 |
| Attack damage | 42 |
| Armor | 30 |
| Attack speed | 0.9 |
| Move speed | 11.5 |
| Range | 15.0 |

## Kit

### Q — Vinebind

- Targeting: Target, range 14.0
- Cooldown: 210, 70 cost
- Effects: stun 40 ticks; 40 damage (+40% AD)

### W — Petal Storm

- Targeting: Area, range 14.0
- Cooldown: 150, 60 cost
- Effects: 55 damage (+40% AD); slow 90 ticks

### R — Mending Verse

- Targeting: SelfCast
- Cooldown: 140, 50 cost
- Effects: heal 120

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies support growth
across levels 1-18. Deploy power for the Rare card is ~235.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

