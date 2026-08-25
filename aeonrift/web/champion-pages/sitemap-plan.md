# Sitemap plan — champion-pages

**SCAFFOLD.** How generated champion pages roll up into a crawlable sitemap.

## Structure

```
https://ereboslab.com/
├── /champions                     (index — links every champion)
├── /champions/<slug>              (one page per content/champions/*.ron)
└── sitemap.xml                    (generated; lists all champion URLs + lastmod)
```

## Generation

1. Enumerate `content/champions/*.ron` (the loadable specs — read-only source of truth).
2. Project each to a `seo-model.ron` instance (see [seo-model.ron](./seo-model.ron)).
3. Render `page-template.html` per champion → static HTML.
4. Emit `sitemap.xml` with one `<url>` per champion, `lastmod` = spec file git mtime,
   `changefreq` = `weekly`, `priority` = `0.7` (roster index gets `0.9`).

## Rules

- A URL enters the sitemap **only** after its page renders cleanly (no missing placeholders).
- Slugs are stable and match the RON `id` / filename — never renamed once published (301 if forced).
- The sitemap is regenerated on every content change and every patch, then submitted to search consoles.
