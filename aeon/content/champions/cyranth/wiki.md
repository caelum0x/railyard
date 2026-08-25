# Cyranth

> Design notes for the `cyranth` champion bundle. Combat numbers live in the
> flat spec `content/champions/cyranth.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Tide
- **Archetype:** marksman
- **Ascendancy card:** Epic, 6 elixir

## Lore

Cyranth — a MARKSMAN. High sustained auto DPS, a hard-hitting skillshot, and a single-target slow to kite. Authored purely as DATA against the closed DSL.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 585 |
| Attack damage | 63 |
| Armor | 24 |
| Attack speed | 1.15 |
| Move speed | 11.5 |
| Range | 20.0 |

## Kit

### Q — Squall Volley

- Targeting: Skillshot, range 22.0
- Cooldown: 90, 45 cost
- Effects: 90 damage (+110% AD)

### W — Sleetbind Shot

- Targeting: Target, range 20.0
- Cooldown: 150, 50 cost
- Effects: 50 damage (+50% AD); slow 75 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies marksman growth
across levels 1-18. Deploy power for the Epic card is ~322.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

