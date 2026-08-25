# tests/content

**SCAFFOLD.** Cross-crate content validation tests. Not a compiled crate; not a Cargo
workspace member. See ../../MIGRATION.md.

## What this guards

Every champion spec in `content/champions/*.ron` is **loadable, lintable, and playable**:

1. **Schema** — matches `content/_schema/champion.schema.ron` + `ability.schema.ron`.
2. **Closed vocabulary** — every action/shape/filter/damage_type/buff is a member of
   `content/_schema/vocabulary.ron` (per MOBA-ARCHITECTURE law 2 — unknown primitive = reject).
3. **Assets resolve** — each spec's `assets_manifest` resolves to a valid glTF-2 binary via
   `assets-manifests/` (by blake3, never a binary in git).
4. **Duel gate** — the spec's `duel_tests` scenarios run deterministically to their expected result.

The engine-side linter lives in `tools/content-lint`; this directory holds the cross-crate
harness config + fixtures (see [sample-check.ron](./sample-check.ron)) that runs it over the
whole roster in CI.

## Failure = red gate

An unknown primitive, an unresolved asset, or a schema mismatch fails CI. Content is data,
and invalid data never merges.
