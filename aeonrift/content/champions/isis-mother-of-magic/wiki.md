# Isis, Mother Of Magic

> Design notes for the `isis-mother-of-magic` champion bundle. Combat numbers live in the
> flat spec `content/champions/isis-mother-of-magic.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Egyptian
- **Archetype:** mage
- **Ascendancy card:** Common, 3 elixir

## Lore

Isis — a SUPPORT built on a huge self-heal and enemy lockdown. Modest damage; her value is the Healing Ankh's large restore, a Binding Spell that stuns, and Sorrow's Veil that blankets an area in a long slow. Peel and sustain, expressed purely as Heal / Stun / Slow.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 540 |
| Attack damage | 30 |
| Armor | 22 |
| Attack speed | 0.7 |
| Move speed | 9.5 |
| Range | 7.0 |

## Kit

### Q — Healing Ankh

- Targeting: SelfCast
- Cooldown: 120, 60 cost
- Effects: heal 160

### W — Binding Spell

- Targeting: Target, range 18.0
- Cooldown: 60, 50 cost
- Effects: 45 damage (+30% AD); stun 12 ticks

### R — Sorrow's Veil

- Targeting: Area, range 16.0
- Cooldown: 50, 40 cost
- Effects: 35 damage (+20% AD); slow 20 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Common card is ~188.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

