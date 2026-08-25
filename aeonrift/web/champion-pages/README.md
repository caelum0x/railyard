# web/champion-pages

**SCAFFOLD.** ereboslab.com integration — one SEO landing page per champion.
Not a compiled crate; not a Cargo workspace member. MIGRATE: ereboslab.com scaffold
(see ../../MIGRATION.md).

## Role

Generates and serves one crawlable HTML page per champion in `content/champions/*.ron`.
Each page is data-driven: the SEO model in [seo-model.ron](./seo-model.ron) is projected
from the champion spec (name, title, lore_tag, abilities) so the roster and the marketing
site never drift. New champion RON → new page, automatically.

## Pipeline

```
content/champions/<slug>.ron  ──▶  seo-model (projection)  ──▶  page-template.html  ──▶  static HTML on CDN
```

- `content/champions/*.ron` is the **source of truth** (owned by the content area — read-only here).
- `seo-model.ron` documents the fields each page needs and where they come from.
- `page-template.html` is the render skeleton (placeholders in `{{ ... }}`).
- `sitemap-plan.md` describes how pages roll up into `sitemap.xml` for indexing.

## Laws inherited

- **Original IP only** — copy and imagery reference original-IP champions, never a third party's.
- **No binaries in git** — hero art is referenced by hash and served from the CDN.
- **Content parity** — a champion page must not claim abilities the RON spec does not define.
