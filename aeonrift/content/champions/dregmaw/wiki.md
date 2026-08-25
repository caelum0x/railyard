# Dregmaw

> Design notes for the `dregmaw` champion bundle. Combat numbers live in the
> flat spec `content/champions/dregmaw.ron`; this file is human-facing rationale.

## Identity

- **Faction:** Umbral
- **Archetype:** tank
- **Ascendancy card:** Epic, 5 elixir

## Lore

Dregmaw — the DUSKBULWARK, a frontline tank. (Umbral faction) A hollow-eyed warden fused to a slab of eclipse-iron, Dregmaw plants himself in the dark and refuses to fall. Soaks punishment, chains a threat down with an umbral grip, mends in shadow, and drags a whole line into a crawl. Low offense, immense durability. Pure DATA against the closed DSL.

## Base stats

| Stat | Value |
| --- | --- |
| HP | 690 |
| Attack damage | 46 |
| Armor | 37 |
| Attack speed | 0.85 |
| Move speed | 10.5 |
| Range | 6.0 |

## Kit

### Q — Umbral Seize

- Targeting: Target, range 9.0
- Cooldown: 240, 60 cost
- Effects: stun 45 ticks; 65 damage (+40% AD)

### W — Eclipse Skin

- Targeting: SelfCast
- Cooldown: 180, 50 cost
- Effects: heal 105

### R — Duskwave

- Targeting: Area, range 11.0
- Cooldown: 210, 55 cost
- Effects: 60 damage (+40% AD); slow 90 ticks

## Balance rationale

`tuning.ron` ranks each ability 1-5: damage/heal grow ~25-30%% per rank while
cooldowns trim ~5%% per rank and costs rise ~8%% per rank, so investing points
raises ceiling and tempo cost together. `scaling.ron` applies tank growth
across levels 1-18. Deploy power for the Epic card is ~266.

## Duel baselines

`duel_tests.ron` pins three deterministic scenarios (seed 42): a baseline
matchup, a determinism check (same seed twice = identical hash), and an
order-independence check. See `engine/ability-dsl/tests/duel.rs` for the runner.

