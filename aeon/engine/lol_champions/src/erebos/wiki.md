# Erebos, the Eclipse Warden

**Original IP.** Ties to the *ereboslab* studio brand. "Erebos" is a public-domain
primordial-night name; this champion, its kit, and all values are original and are
**not** copied from any Riot champion.

## Fantasy

A warden of the long dark who fights by drawing the shadow between things and, at the
climax, swallowing the sun. Deliberately simple, legal, and readable — built to prove
the fork's original-content path end to end.

## Kit

Every ability is composed **only** from the closed `lol_core` action/buff vocabulary
(see `content/_schema/vocabulary.ron`).

| Slot | Name | Composed from (`lol_core`) | Effect |
|------|------|----------------------------|--------|
| Passive | **Umbral Gathering** | `EventDamageCreate` → `BuffEclipseMark` | Erebos' ability damage marks the target (Eclipse Mark). |
| Q | **Shadowlance** | `ActionDamage` + `DamageShape::Rectangle` | Line skillshot dealing Magic damage (`q_damage`). |
| W | **Veil of Dusk** | `BuffMoveSpeed` + `BuffSelfHeal` | Self swiftness (+30% MS, 2.5s) and a mending pulse. |
| E | **Umbral Step** | `ActionDash` + `DashMoveType::Pointer` | Blink toward the cursor, clamped to max range. |
| R | **Eclipse** | `ActionDamage` (`Circle`) + `ActionDisplace` (`DisplaceEffect::Stun`) | Darkness erupts around Erebos: Magic damage to nearby champions (`r_damage`) and a stun. |

## Files

- Code: `crates/lol_champions/src/erebos/{mod.rs, buffs.rs, tests.rs}` — the Bevy
  `PluginErebos`, following the `ahri/` pattern.
- Data: `content/champions/erebos/{spec, tuning, duel_tests, assets.manifest}.ron`.
- Registered in `crates/lol_champions/src/lib.rs` (`PluginChampions`).

## Determinism

The game path uses **no RNG**; combined with the engine's fixed-timestep sim, the
same inputs produce bit-identical results. `tests.rs` proves this
(`erebos_q_is_deterministic_across_runs`) and runs a minimal duel/skill-cast gate for
Q (damage + mark), W (self swiftness), and R (stun + damage) — all headless, with an
**in-memory `Spell`** and **zero Riot assets**.
