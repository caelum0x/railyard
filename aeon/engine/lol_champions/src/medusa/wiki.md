# Medusa

**Graduated original content.** Ported from the Aeonrift min-ECS DSL
(`content/champions/medusa.ron`) onto `lol_core` per `MAP.md`. Public-domain name;
original kit and values.

## Fantasy

A fast, long-range marksman whose gaze turns a target to stone (a long stun),
backed by a venom volley that slows on impact.

## Kit

| Slot | Name | DSL targeting/effects | Composed from (`lol_core`) |
|------|------|-----------------------|----------------------------|
| Q | **Petrifying Gaze** | Skillshot: Damage + Stun | `ActionDamage` (`Rectangle`) + `ActionDisplace` (`Stun`) |
| W | **Venom Volley** | Skillshot: Damage + Slow | `ActionDamage` (`Rectangle`) + `ActionDisplace` (`Slow`) |

DamageType `Physical` (marksman).

## Files

- Code: `crates/lol_champions/src/medusa/{mod.rs, tests.rs}` — `PluginMedusa`,
  registered in `PluginChampions`.
- Source DSL: `aeonrift/content/champions/medusa.ron`.

## Determinism

No RNG on the game path; `tests.rs` proves Q damage+stun, cross-run determinism,
and W damage+slow — headless, in-memory `Spell`, zero Riot assets.
