# Lumisel

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/lumisel.ron`) onto `lol_core` per `MAP.md`. Invented fantasy
name (Tide faction); original kit and values.

## Fantasy

An enchanter support: two heals of different weights plus a slowing, chip-damage
zone for peel. Low personal damage by design.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Tidal Blessing** | SelfCast: Heal | `BuffSelfHeal` |
| W | **Frostwater Veil** | Area: Slow + Damage | `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`) |
| E | **Riptide Ward** | SelfCast: Heal | `BuffSelfHeal` |

DamageType `Physical` (support).

## Files

- Code: `crates/lol_champions/src/lumisel/{mod.rs, tests.rs}` — `PluginLumisel`,
  registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/lumisel.ron`.

## Determinism

No RNG; `tests.rs` proves W damage+slow, cross-run determinism, and Q self-heal —
headless, in-memory `Spell`, zero Riot assets.
