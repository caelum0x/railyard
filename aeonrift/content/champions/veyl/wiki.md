# Veyl

> Design notes for the `veyl` champion bundle. Combat numbers live in the
> flat spec `content/champions/veyl.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Tide
- **Archetype:** assassin
- **Ascendancy card:** Legendary, 7 elixir

## Lore

Veyl — an ASSASSIN. Gap-closing dash burst plus a lockdown execute. Squishy but mobile and high-damage. Pure DATA against the closed DSL.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 620 |
| Attack damage | 70 |
| Armor | 32 |
| Attack speed | 1.0 |
| Move speed | 12.0 |
| Range | 6.0 |

## Kit

### Q — Undertow Lunge

- Targeting: Skillshot, range 16.0
- Cooldown: 150, 40 cost
- Effects: dash 16; 150 damage (+110% AD)

### W — Drowning Grip

- Targeting: Target, range 7.0
- Cooldown: 210, 60 cost
- Effects: stun 48 ticks; 120 damage (+90% AD)

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies assassin growth
across levels 1-18. Deploy power for the Legendary card is ~424.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

