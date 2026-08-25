# web/patch-notes

**SCAFFOLD.** ereboslab.com patch-notes surface. Not a compiled crate; not a Cargo
workspace member. See ../../MIGRATION.md.

## Role

Renders human-readable patch notes from balance changes. Each patch is a Markdown file
named by version (`YYYY.MM.N.md`); the store, champion-pages footer, and in-client news
feed all link to the latest one. Numeric balance deltas should be derived from diffs of
`content/balance/*` so the notes match what actually shipped.

## Files

- [template.md](./template.md) — the patch-note authoring template.
- [2026.08.0.md](./2026.08.0.md) — a sample patch note.

## Convention

- Version = `YYYY.MM.N` (year, month, patch index within month), matching the patcher channel head.
- One file per patch; never edit a shipped note (append a correction section instead).
- Champion names link to `/champions/<slug>`.
