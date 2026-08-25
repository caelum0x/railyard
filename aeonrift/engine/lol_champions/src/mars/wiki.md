# Mars

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/mars.ron`) onto `lol_core` per `MAP.md`. Public-domain Roman
name; original kit and values.

## Fantasy

A disciplined war bruiser: hurl forward to pin a target, sweep the legion's advance
across a zone, and sustain through bloodlust.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Spear of Conquest** | Target: Dash + Damage + Stun | `ActionDash` (`Pointer`) + `ActionDamage` (`Nearest`) + `ActionDisplace` (`Stun`) |
| W | **Crimson March** | Area: Damage + Slow | `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`) |
| E | **Battle Fury** | SelfCast: Heal | `BuffSelfHeal` |

DamageType `Physical` (bruiser).

## Files

- Code: `crates/lol_champions/src/mars/{mod.rs, tests.rs}` — `PluginMars`,
  registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/mars.ron`.

## Determinism

No RNG; `tests.rs` proves Q dash+damage+stun, cross-run determinism, and E
self-heal — headless, in-memory `Spell`, zero Riot assets.
