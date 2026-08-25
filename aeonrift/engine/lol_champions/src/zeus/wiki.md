# Zeus, King of Olympus

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/zeus.ron`) onto the real `lol_core` engine per `MAP.md`.
"Zeus" is a public-domain mythological name; the kit and all values are original.

## Fantasy

A burst mage who calls down a long-range bolt to petrify a foe in place, then
detonates a storm that scatters and cripples everyone nearby.

## Kit (composed only from the closed `lol_core` vocabulary)

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Thunderbolt** | Skillshot: Damage + Stun | `ActionDamage` (`Rectangle`, Magic) + `ActionDisplace` (`Stun`) |
| W | **Storm's Wrath** | Area: Damage + Slow | `ActionDamage` (`Circle`, Magic) + `ActionDisplace` (`Slow`) |

DamageType is `Magic` (mage archetype per `roster_meta.ron`).

## Files

- Code: `crates/lol_champions/src/zeus/{mod.rs, tests.rs}` — Bevy `PluginZeus`,
  following the `erebos/` pattern. Registered in `PluginChampions` (`lib.rs`).
- Source DSL: `aeonrift/content/champions/zeus.ron`.

## Determinism

The game path uses no RNG; on the fixed-timestep sim the same inputs yield
bit-identical results. `tests.rs` proves damage+stun, cross-run determinism, and
AoE damage+slow — all headless with an in-memory `Spell` and zero Riot assets.
