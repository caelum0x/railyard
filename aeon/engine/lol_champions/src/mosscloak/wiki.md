# Mosscloak

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/mosscloak.ron`) onto `lol_core` per `MAP.md`. Invented fantasy
name (Verdant faction); original kit and values.

## Fantasy

A frontline bruiser who soaks damage, roots a single threat, regrows his own bark,
and cripples everything nearby with a coil of thorns.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Root Snare** | Target: Stun + Damage | `ActionDamage` (`Nearest`) + `ActionDisplace` (`Stun`) |
| W | **Barkskin** | SelfCast: Heal | `BuffSelfHeal` |
| E | **Thorn Coil** | Area: Damage + Slow | `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`) |

DamageType `Physical` (bruiser).

## Files

- Code: `crates/lol_champions/src/mosscloak/{mod.rs, tests.rs}` — `PluginMosscloak`,
  registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/mosscloak.ron`.

## Determinism

No RNG; `tests.rs` proves Q damage+stun, cross-run determinism, W self-heal, and E
damage+slow — headless, in-memory `Spell`, zero Riot assets.
