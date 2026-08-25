# content/monsters

Jungle camp definitions. Individual monster units (`*-sentinel.ron`, `gloomwolf-*`,
`thornhusk-*`, `skyfin-*`, `bogwallow.ron`, `currentback-scuttler.ron`) are grouped
into camps by the `camp-*.ron` files. Buff camps grant the jungle buffs defined in
`jungle-buffs.ron`; all camps scale via `monster-scaling.ron`. Camp placements and
respawn timing live in `../maps/jungle-layout-riftline.ron` and the map file.

Closed vocabulary lives in `content/_schema/vocabulary.ron` (copied canonical from
the moon-lol fork/original-ip). All `.ron` here is CI-gated by `tools/content-lint`
against `_schema`.
