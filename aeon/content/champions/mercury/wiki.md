# Mercury

> Design notes for the `mercury` champion bundle. Combat numbers live in the
> flat spec `content/champions/mercury.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Roman
- **Archetype:** assassin
- **Ascendancy card:** Epic, 6 elixir

## Lore

Mercury — the speed ASSASSIN. Blistering move speed, dash-in burst, dash-out escape.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 520 |
| Attack damage | 58 |
| Armor | 20 |
| Attack speed | 1.0 |
| Move speed | 13.0 |
| Range | 5.0 |

## Kit

### Q — Winged Strike

- Targeting: Target, range 16.0
- Cooldown: 50, 40 cost
- Effects: dash 16; 95 damage (+70% AD)

### W — Caduceus Flurry

- Targeting: Skillshot, range 18.0
- Cooldown: 60, 45 cost
- Effects: 60 damage (+50% AD); slow 30 ticks

### R — Fleet Escape

- Targeting: SelfCast
- Cooldown: 40, 25 cost
- Effects: dash 12

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies assassin growth
across levels 1-18. Deploy power for the Epic card is ~311.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

