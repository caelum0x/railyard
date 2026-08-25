# Tyr

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/tyr.ron`) onto `lol_core` per `MAP.md`. Public-domain Norse
name; original kit and values.

## Fantasy

A stalwart tank — the god of law and sacrifice — who binds a single foe, cripples
a whole zone, and endures through a heavy self-heal so allies don't have to.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Oath of the Bound** | Target: Damage + Stun | `ActionDamage` (`Nearest`) + `ActionDisplace` (`Stun`) |
| W | **Warden's Toll** | Area: Damage + Slow | `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`) |
| E | **Unyielding Sacrifice** | SelfCast: Heal | `BuffSelfHeal` |

DamageType `Physical` (tank).

## Files

- Code: `crates/lol_champions/src/tyr/{mod.rs, tests.rs}` — `PluginTyr`,
  registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/tyr.ron`.

## Determinism

No RNG; `tests.rs` proves Q damage+stun, cross-run determinism, W damage+slow, and
E self-heal — headless, in-memory `Spell`, zero Riot assets.
