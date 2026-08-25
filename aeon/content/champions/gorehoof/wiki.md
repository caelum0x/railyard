# Gorehoof

> Design notes for the `gorehoof` champion bundle. Combat numbers live in the
> flat spec `content/champions/gorehoof.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Verdant
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 5 elixir

## Lore

Gorehoof — a diving BRUISER. Charges in, staggers the target on impact, trades blows, and drinks from bloodroot to stay in the brawl. Authored entirely as DSL data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 640 |
| Attack damage | 62 |
| Armor | 32 |
| Attack speed | 1.0 |
| Move speed | 12.0 |
| Range | 8.0 |

## Kit

### Q — Rampart Charge

- Targeting: Skillshot, range 14.0
- Cooldown: 300, no cost
- Effects: dash 12; 60 damage (+50% AD); stun 30 ticks

### W — Gore Toss

- Targeting: Target, range 9.0
- Cooldown: 120, no cost
- Effects: 45 damage (+80% AD)

### R — Bloodroot

- Targeting: SelfCast
- Cooldown: 160, no cost
- Effects: heal 85

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~285.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

