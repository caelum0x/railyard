# content/_schema — closed action/effect/buff vocabulary

These RON files define the **closed set** of primitives a champion spec may
reference. They are the ground truth extracted from the engine
(`crates/lol_core`) — a content-lint can validate every `content/champions/*/spec.ron`
against them and **reject any unknown effect / shape / damage-type / buff** (red gate,
per MOBA-ARCHITECTURE.md §7 law 2).

- `vocabulary.ron` — the enumerated closed set (actions, shapes, filters, damage
  types, CC debuffs, common buffs, dash move types, displace primitives).
- `champion.schema.ron` — the shape of a champion `spec.ron`.
- `ability.schema.ron` — the shape of one ability entry inside a spec.

Source of truth in code:

| Vocabulary group | Rust source |
|------------------|-------------|
| Actions          | `crates/lol_core/src/action/{damage,dash,displace,knockback,delayed_damage}.rs` |
| Damage shapes    | `crates/lol_core/src/action/damage.rs` (`DamageShape`) |
| Target filters   | `crates/lol_core/src/action/damage.rs` (`TargetFilter`) |
| Damage modifiers | `crates/lol_core/src/action/damage.rs` (`DamageModifier`) |
| Damage types     | `crates/lol_core/src/damage.rs` (`DamageType`) |
| CC debuffs       | `crates/lol_core/src/buffs/cc_debuffs.rs` |
| Common buffs     | `crates/lol_core/src/buffs/common_buffs.rs` + `shield_white/shield_magic/damage_reduction` |
| Skill slots      | `crates/lol_core/src/skill/enums.rs` (`SkillSlot`) |

If code adds a primitive, add it here in the same change, or the linter rejects
specs that use it.
