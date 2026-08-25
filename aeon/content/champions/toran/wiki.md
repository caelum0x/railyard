# Toran

> Design notes for the `toran` champion bundle. Combat numbers live in the
> flat spec `content/champions/toran.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Ember
- **Archetype:** mage
- **Ascendancy card:** Epic, 5 elixir

## Lore

Toran — the SLAGWARDEN, a frontline tank. Soaks damage, anchors fights with a heavy stun, and cools himself with a forge-fed self-heal. Low offense, immense durability. Pure DATA — no champion-specific Rust behind it.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 690 |
| Attack damage | 44 |
| Armor | 36 |
| Attack speed | 0.85 |
| Move speed | 10.5 |
| Range | 8.0 |

## Kit

### Q — Anvil Slam

- Targeting: Target, range 10.0
- Cooldown: 240, 60 cost
- Effects: 70 damage (+40% AD); stun 45 ticks

### W — Molten Ward

- Targeting: SelfCast
- Cooldown: 180, 50 cost
- Effects: heal 100

### R — Cinder Sweep

- Targeting: Area, range 12.0
- Cooldown: 300, 70 cost
- Effects: 90 damage (+50% AD); slow 90 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies mage growth
across levels 1-18. Deploy power for the Epic card is ~297.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

