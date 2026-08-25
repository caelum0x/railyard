# Loki

> Design notes for the `loki` champion bundle. Combat numbers live in the
> flat spec `content/champions/loki.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Norse
- **Archetype:** assassin
- **Ascendancy card:** Epic, 5 elixir

## Lore

Loki — a trickster ASSASSIN: blink to a mark, gut them, slip away. Squishy but fast, with the highest single-hit burst on the roster. Shadow Step dashes onto a target and knifes for a huge AD ratio; a venom skillshot slows stragglers; and a vanishing self-heal buys the escape. All closed-vocab data.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 520 |
| Attack damage | 54 |
| Armor | 22 |
| Attack speed | 1.0 |
| Move speed | 12.0 |
| Range | 5.0 |

## Kit

### Q — Shadow Step

- Targeting: Target, range 16.0
- Cooldown: 45, 50 cost
- Effects: dash 16; 80 damage (+50% AD); slow 10 ticks

### W — Serpent's Venom

- Targeting: Skillshot, range 20.0
- Cooldown: 30, 35 cost
- Effects: 55 damage (+40% AD); slow 16 ticks

### R — Vanishing Trick

- Targeting: SelfCast
- Cooldown: 120, no cost
- Effects: heal 70

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies assassin growth
across levels 1-18. Deploy power for the Epic card is ~282.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

