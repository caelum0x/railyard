# Sentinel

> Design notes for the `sentinel` champion bundle. Combat numbers live in the
> flat spec `content/champions/sentinel.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Umbral
- **Archetype:** marksman
- **Ascendancy card:** Common, 3 elixir

## Lore

Sentinel — a plain AUTO-ATTACKER. No burst, no crowd control: just reliable, hard-hitting basic attacks and a modest self-heal. The baseline every other champion is measured against. This whole champion is DATA — there is zero champion-specific Rust code behind it.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 620 |
| Attack damage | 55 |
| Armor | 30 |
| Attack speed | 1.0 |
| Move speed | 11.0 |
| Range | 8.0 |

## Kit

### Q — Second Wind

- Targeting: SelfCast
- Cooldown: 150, no cost
- Effects: heal 90

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies marksman growth
across levels 1-18. Deploy power for the Common card is ~184.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

