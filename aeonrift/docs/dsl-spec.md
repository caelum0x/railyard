# Ability DSL — specification

**SCAFFOLD.** The data model for champions and abilities. Ground truth is
`content/_schema/` (schemas + `vocabulary.ron`); this doc explains it.

## Why a DSL

Champions are **data, not code**. A champion is a `.ron` spec that references a **closed set**
of engine-implemented primitives. This keeps content inside the engine's real capabilities and
lets `tools/content-lint` reject anything the engine can't actually do (a red CI gate).

## Files

- `content/_schema/champion.schema.ron` — shape of a champion spec.
- `content/_schema/ability.schema.ron` — shape of one ability.
- `content/_schema/vocabulary.ron` — the **closed set**: actions, damage shapes, target
  filters, damage types/modifiers, dash move types, displace primitives, CC + common buffs,
  skill slots, cooldown modes.

## A champion spec

A champion is `(name, base stats, [abilities])`. Each ability is
`(name, cooldown, cost, targeting, range, [effects])` where every effect is a member of the
closed vocabulary. See a live example in `content/champions/achilles.ron`:

```ron
AbilitySpec(
    name: "Spear of Peleus",
    cooldown: 85,
    cost: Some(45),
    targeting: Target,
    range: 13.0,
    effects: [
        Dash(distance: 13.0),
        Damage(amount: 46, scaling: Some(AttackDamage(ratio: 0.35))),
        Stun(duration: 8),
    ],
)
```

## The closed-vocabulary law

Every `action`, `shape`, `filter`, `damage_type`, and `buff` a spec names **must** exist in
`vocabulary.ron`. If the engine gains a primitive, it is added to `vocabulary.ron` in the same
change; otherwise the linter rejects any spec that uses it. This is
MOBA-ARCHITECTURE §7 law 2.

## Validation pipeline

```
content/champions/*.ron
   → engine/ability-dsl loader (parse)
   → tools/content-lint (schema + closed-vocab + assets resolve)
   → duel gate (deterministic scenarios reach expected result)
```

See [authoring-guide.md](./authoring-guide.md) for how to write one, and `tests/content/`
for the CI gate.
