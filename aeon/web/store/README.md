# web/store

**SCAFFOLD.** ereboslab.com storefront surface. Not a compiled crate; not a Cargo
workspace member. See ../../MIGRATION.md.

## Role

The web-facing catalog + checkout entry point. Reads the SKU catalog, renders offers,
and hands confirmed carts to `services/economy` (`POST /v1/orders`). The store **never**
mutates balances or inventory itself — it is a thin, cache-friendly view over the
authoritative economy/inventory services.

## Files

- [catalog.sample.ron](./catalog.sample.ron) — a sample catalog snapshot (shape of a SKU listing).
- [pricing-tiers.md](./pricing-tiers.md) — the currency + price-tier model.

## Flow

```
store (browse catalog) ──▶ economy POST /v1/orders ──▶ economy settles ──▶ inventory fulfils grant
```

## Laws inherited

- **Server-authoritative** — prices shown are validated server-side at order time; the client price is advisory.
- **Original IP only** — every SKU maps to an original-IP champion/skin.
- **No binaries in git** — thumbnails are hash-referenced and CDN-served.
