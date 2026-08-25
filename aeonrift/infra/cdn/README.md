# infra/cdn

**SCAFFOLD.** CDN + object-store configuration for content delivery. Illustrative stubs.
See ../../MIGRATION.md.

## Role

Describes how the edge serves the two big content flows — **patch chunks** (content-addressed
by blake3, immutable, cached forever) and **web static assets** (champion pages, store art) —
plus the R2 bucket layout that backs them. Upholds the no-binaries-in-git law: git holds
manifests and hashes; the CDN + object store hold the bytes.

## Files

- [cache-rules.json](./cache-rules.json) — per-path caching policy (TTL, immutability).
- [r2-buckets.md](./r2-buckets.md) — bucket layout, lifecycle, and access model.

## Principles

- Content-addressed chunks are **immutable** → `Cache-Control: immutable, max-age=1y`.
- HTML is short-TTL + revalidated so patch-note/champion updates propagate quickly.
- Origin is the object store; the patcher only ever hands out hashes, never bytes.
