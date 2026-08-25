# Set, Lord Of Chaos

> Design notes for the `set-lord-of-chaos` champion bundle. Combat numbers live in the
> flat spec `content/champions/set-lord-of-chaos.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Egyptian
- **Archetype:** assassin
- **Ascendancy card:** Epic, 5 elixir

## Lore

Set — an ASSASSIN: long dash engage, single-target burst, and a chaotic escape. Squishier and faster than a bruiser, with the highest per-hit burst here. Desert Ambush leaps a huge distance and detonates on the target; Sandstorm cleans up and slows; Chaos Step repositions and heals to survive the dive.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 560 |
| Attack damage | 43 |
| Armor | 22 |
| Attack speed | 0.95 |
| Move speed | 12.0 |
| Range | 5.0 |

## Kit

### Q — Desert Ambush

- Targeting: Target, range 18.0
- Cooldown: 40, 30 cost
- Effects: dash 18; 76 damage (+50% AD)

### W — Sandstorm

- Targeting: Area, range 14.0
- Cooldown: 60, 45 cost
- Effects: 58 damage (+35% AD); slow 14 ticks

### R — Chaos Step

- Targeting: SelfCast
- Cooldown: 100, no cost
- Effects: dash 12; heal 60

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies assassin growth
across levels 1-18. Deploy power for the Epic card is ~260.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

