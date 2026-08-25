# Loki

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/loki.ron`) onto `lol_core` per `MAP.md`. Public-domain Norse
name; original kit and values.

## Fantasy

A trickster assassin: blink onto a mark, gut them, and slip away. Squishy but fast,
with the highest single-hit burst on the roster.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Shadow Step** | Target: Dash + Damage + Slow | `ActionDash` (`Pointer`) + `ActionDamage` (`Nearest`) + `ActionDisplace` (`Slow`) |
| W | **Serpent's Venom** | Skillshot: Damage + Slow | `ActionDamage` (`Rectangle`) + `ActionDisplace` (`Slow`) |
| E | **Vanishing Trick** | SelfCast: Heal | `BuffSelfHeal` |

DamageType `Physical` (assassin).

## Files

- Code: `crates/lol_champions/src/loki/{mod.rs, tests.rs}` — `PluginLoki`,
  registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/loki.ron`.

## Determinism

No RNG; `tests.rs` proves Q dash+damage+slow, cross-run determinism, and E
self-heal — headless, in-memory `Spell`, zero Riot assets.
