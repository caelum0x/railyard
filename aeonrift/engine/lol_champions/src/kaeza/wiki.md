# Kaeza, the Emberlock

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/kaeza.ron`) onto `lol_core` per `MAP.md`. Invented fantasy
name (Ember faction); original kit and values.

## Fantasy

A ranged marksman with sustained DPS: a piercing scorch bolt, a repositioning hop,
and a heavy single-target round that slows.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Scorch Bolt** | Skillshot: Damage | `ActionDamage` (`Rectangle`) |
| W | **Ashstep** | Skillshot: Dash | `ActionDash` (`Pointer`) |
| E | **Slagshot** | Target: Damage + Slow | `ActionDamage` (`Nearest`) + `ActionDisplace` (`Slow`) |

DamageType `Physical` (marksman).

## Files

- Code: `crates/lol_champions/src/kaeza/{mod.rs, tests.rs}` — `PluginKaeza`,
  registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/kaeza.ron`.

## Determinism

No RNG; `tests.rs` proves Q line damage, cross-run determinism, and E damage+slow —
headless, in-memory `Spell`, zero Riot assets.
