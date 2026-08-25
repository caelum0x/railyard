# content

Data layer — the roster is the bulk of the franchise file count. All `.ron` here is
CI-gated by `tools/content-lint` against the closed vocabulary in
[`_schema/vocabulary.ron`](./_schema/vocabulary.ron) (copied canonical from the
moon-lol `fork/original-ip`). Unknown effect/targeting = red (Law 2).

## One-dir-per-champion convention
Each champion is a self-contained bundle: **one directory per champion**, slug-named,
that drives BOTH games (author once, ship twice).

```
content/champions/<name>/
  champion.ron        # identity, faction/pantheon, base stats
  abilities/{passive,q,w,e,r}.ron
  tuning.ron  scaling.ron
  duel_tests.ron      # determinism gate (Law 3)
  card_meta.ron       # cost/rarity -> turns the champion into an Ascendancy card
  vo.ron
  assets.manifest.ron # hash->path; blobs live in the asset-store, not git (Law 4)
  meta.ron  wiki.md
```

- `champion.ron` + `abilities/` -> compiled by `engine/ability-dsl` for the MOBA.
- `card_meta.ron` -> `tools/card-gen` derives the `games/aeonrift-ascendancy` card.
- Unique slug per champion; original IP only (Law 5).

Reference bundle available in the moon-lol fork: `content/champions/erebos/`.
Other content types (`items/ runes/ maps/ minions/ balance/ cards/ i18n/`) follow
the same schema-validated, one-file-per-entity convention.
