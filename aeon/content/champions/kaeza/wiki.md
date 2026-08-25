# Kaeza

> Design notes for the `kaeza` champion bundle. Combat numbers live in the
> flat spec `content/champions/kaeza.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Ember
- **Archetype:** marksman
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Kaeza — the EMBERLOCK, a ranged marksman. Sustained ranged DPS with a piercing bolt and a repositioning hop. High auto-attack range, thin defenses. Authored as DATA only.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 560 |
| Attack damage | 56 |
| Armor | 22 |
| Attack speed | 1.1 |
| Move speed | 11.0 |
| Range | 25.0 |

## Kit

### Q — Scorch Bolt

- Targeting: Skillshot, range 30.0
- Cooldown: 90, 40 cost
- Effects: 70 damage (+100% AD)

### W — Ashstep

- Targeting: Skillshot, range 14.0
- Cooldown: 150, 35 cost
- Effects: dash 14

### R — Slagshot

- Targeting: Target, range 26.0
- Cooldown: 300, 80 cost
- Effects: 140 damage (+100% AD); slow 60 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies marksman growth
across levels 1-18. Deploy power for the Legendary card is ~375.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

