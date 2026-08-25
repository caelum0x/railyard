# Quorvane

> Design notes for the `quorvane` champion bundle. Combat numbers live in the
> flat spec `content/champions/quorvane.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Umbral
- **Archetype:** marksman
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Quorvane — the NIGHTGLASS HUNTER, a marksman. (Umbral faction) A patient stalker who fires shards of cooled void-glass from the edge of sight. Long range and high fire-rate, thin defenses; slips through shadow to keep the gap and pins fleeing prey with a heavy piercing round. DSL data only.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 560 |
| Attack damage | 58 |
| Armor | 22 |
| Attack speed | 1.15 |
| Move speed | 11.0 |
| Range | 24.0 |

## Kit

### Q — Umbral Bolt

- Targeting: Skillshot, range 28.0
- Cooldown: 90, 40 cost
- Effects: 70 damage (+100% AD)

### W — Shadowslip

- Targeting: Skillshot, range 14.0
- Cooldown: 150, 35 cost
- Effects: dash 14

### R — Nightpiercer

- Targeting: Target, range 24.0
- Cooldown: 300, 80 cost
- Effects: 130 damage (+100% AD); slow 60 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies marksman growth
across levels 1-18. Deploy power for the Legendary card is ~365.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

