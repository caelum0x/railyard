# Champion authoring guide

**SCAFFOLD.** How to add or edit a champion. The rules here are enforced by
`tools/content-lint` + `tests/content/`, so following them means green CI.

## 1. Write the spec

Create `content/champions/<slug>.ron`. Start from an existing spec (e.g.
`content/champions/achilles.ron`) and keep the shape from
`content/_schema/champion.schema.ron`:

```ron
ChampionSpec(
    name: "<Display Name>",
    base: BaseStats( hp: 760, ad: 40, armor: 33, attack_speed: 0.9, move_speed: 10.0, range: 5.0 ),
    abilities: [ /* Passive/Q/W/E/R */ ],
)
```

- `slug` (filename) is stable snake_case and matches the id — never rename once published.
- **Original IP only** — name, title, and lore are original.

## 2. Stay inside the closed vocabulary

Every effect must be a member of `content/_schema/vocabulary.ron`. If you need a primitive
that doesn't exist, that's an **engine change first** — add the primitive to the engine and to
`vocabulary.ron` in the same change, then use it. The linter rejects unknown primitives.

## 3. Balance numbers live in tuning

Structure lives in the spec; tunable numbers belong in `content/balance/*` so a balance pass
is a data diff (and patch notes derive from that diff — see `web/patch-notes`).

## 4. Assets by hash

Reference art via the champion's `assets_manifest`; the binary lives in the content-addressed
store and is resolved by blake3 through `assets-manifests/`. **Never commit a binary.**

## 5. Add duel tests

Give the champion `duel_tests` — deterministic scenarios with expected outcomes. These run in
the content gate so a balance change that breaks a matchup is caught.

## 6. Web is automatic

A new spec automatically yields a champion landing page (`web/champion-pages`) and a store
SKU mapping — no manual web edit needed.

## Checklist

- [ ] Spec matches the schema.
- [ ] Only closed-vocabulary primitives used.
- [ ] Original IP.
- [ ] Assets referenced by hash, no binaries committed.
- [ ] Duel tests present and passing.
- [ ] `tools/content-lint` green locally.
