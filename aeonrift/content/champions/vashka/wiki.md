# Vashka

> Design notes for the `vashka` champion bundle. Combat numbers live in the
> flat spec `content/champions/vashka.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Ember
- **Archetype:** bruiser
- **Ascendancy card:** Epic, 6 elixir

## Lore

Vashka — CINDERMAW, a diving bruiser. Gap-closes with a burning charge, then trades relentlessly with sticky slows and self-sustain. Balanced stats around the sentinel baseline. Authored purely as DATA.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 690 |
| Attack damage | 62 |
| Armor | 34 |
| Attack speed | 0.95 |
| Move speed | 11.5 |
| Range | 8.0 |

## Kit

### Q — Furnace Charge

- Targeting: Skillshot, range 26.0
- Cooldown: 210, 55 cost
- Effects: dash 26; 60 damage (+60% AD)

### W — Grinding Bite

- Targeting: Target, range 9.0
- Cooldown: 150, 45 cost
- Effects: 68 damage (+70% AD); slow 60 ticks

### R — Reforge

- Targeting: SelfCast
- Cooldown: 270, 50 cost
- Effects: heal 90

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies bruiser growth
across levels 1-18. Deploy power for the Epic card is ~302.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

