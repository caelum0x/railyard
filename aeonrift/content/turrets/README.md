# content/turrets

Turret, inhibitor and nexus structure definitions for the MOBA. One file per
structure kind; the Warding Spire tiers (`outer_spire` -> `inner_spire` ->
`inhibitor_spire` -> `nexus_spire`) share the targeting (`spire-targeting.ron`)
and projectile (`spire-projectile.ron`) definitions. The Rift Conduit
(`inhibitor-conduit.ron`) and Aeon Core (`aeon-core.ron`) are the inhibitor and
nexus structures. Placements live in `../maps/riftline-crucible.ron`.

Closed vocabulary lives in `content/_schema/vocabulary.ron` (copied canonical from
the moon-lol fork/original-ip). All `.ron` here is CI-gated by `tools/content-lint`
against `_schema`.
