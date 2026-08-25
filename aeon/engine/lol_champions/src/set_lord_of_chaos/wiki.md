# Set, Lord of Chaos

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/set-lord-of-chaos.ron`) onto `lol_core` per `MAP.md`.
Public-domain Egyptian name; original kit and values.

## Fantasy

An assassin who erupts from the sand onto a target for heavy burst, kicks up a
slowing sandstorm to clean up, then steps through chaos to reposition and mend.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Desert Ambush** | Target: Dash + Damage | `ActionDash` (`Pointer`) + `ActionDamage` (`Nearest`) |
| W | **Sandstorm** | Area: Damage + Slow | `ActionDamage` (`Circle`) + `ActionDisplace` (`Slow`) |
| E | **Chaos Step** | SelfCast: Dash + Heal | `ActionDash` (`Pointer`) + `BuffSelfHeal` |

DamageType `Physical` (assassin).

## Files

- Code: `crates/lol_champions/src/set_lord_of_chaos/{mod.rs, tests.rs}` —
  `PluginSet` (component `Set`), registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/set-lord-of-chaos.ron`.

## Determinism

No RNG; `tests.rs` proves Q dash+damage, cross-run determinism, W damage+slow, and
E self-heal — headless, in-memory `Spell`, zero Riot assets.
