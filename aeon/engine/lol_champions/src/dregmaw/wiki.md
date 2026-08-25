# Dregmaw, the Duskbulwark

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/dregmaw.ron`) onto `lol_core` per `MAP.md`. Invented fantasy
name (Umbral faction); original kit and values.

## Fantasy

A frontline tank fused to a slab of eclipse-iron: soaks punishment, chains a threat
down with an umbral grip, mends in shadow, and drags a whole line into a crawl.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Umbral Seize** | Target: Stun + Damage | `ActionDamage` (`Nearest`) + `ActionDisplace` (`Stun`) |
| W | **Eclipse Skin** | SelfCast: Heal | `BuffSelfHeal` |
| E | **Duskwave** | Area: Damage + Slow | `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`) |

DamageType `Physical` (tank).

## Files

- Code: `crates/lol_champions/src/dregmaw/{mod.rs, tests.rs}` — `PluginDregmaw`,
  registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/dregmaw.ron`.

## Determinism

No RNG; `tests.rs` proves Q damage+stun, cross-run determinism, W self-heal, and E
damage+slow — headless, in-memory `Spell`, zero Riot assets.
