# content/cards/decks

Preset (starter / featured) decks for Aeonrift: Ascendancy's card mode. Each `*.ron`
file is one `PresetDeck` with exactly **8** `champions`.

- Every entry in `champions` is a slug that resolves 1:1 to a file in
  `../../champions/*.ron` (e.g. `"ra-radiant-dawn"` -> `content/champions/ra-radiant-dawn.ron`).
- Decks are either faction-themed (Greek, Norse, Roman, Egyptian, Ember, Tide,
  Verdant, Umbral) or archetype-themed (assassins, mages, tanks/bruisers, marksmen).
- The mean elixir cost of the resolved cards is expected to fall inside the
  `deck_average` window in `../tuning/elixir_curve.ron`.

These are additive content files only — they never modify the champion specs they
reference.
