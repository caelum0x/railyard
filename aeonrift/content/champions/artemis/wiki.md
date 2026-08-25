# Artemis

> Design notes for the `artemis` champion bundle. Combat numbers live in the
> flat spec `content/champions/artemis.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Greek
- **Archetype:** marksman
- **Ascendancy card:** Epic, 5 elixir

## Lore

Artemis — ASSASSIN. Goddess of the hunt: leaps in with a crescent strike for heavy burst, marks fleeing prey with a slowing arrow, and looses a killing volley of moonlight on anyone nearby.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 560 |
| Attack damage | 45 |
| Armor | 22 |
| Attack speed | 1.0 |
| Move speed | 11.5 |
| Range | 8.0 |

## Kit

### Q — Crescent Leap

- Targeting: Target, range 18.0
- Cooldown: 70, 50 cost
- Effects: dash 18; 88 damage (+50% AD)

### W — Hunter's Mark

- Targeting: Skillshot, range 24.0
- Cooldown: 45, 40 cost
- Effects: 75 damage (+50% AD); slow 28 ticks

### R — Moonlit Volley

- Targeting: Area, range 10.0
- Cooldown: 100, 70 cost
- Effects: 90 damage (+55% AD)

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies marksman growth
across levels 1-18. Deploy power for the Epic card is ~283.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

