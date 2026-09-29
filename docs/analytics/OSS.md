# Product analytics: open-source sources (checked with `gh api` and `npm view` on 2026-09-30)

- Each build package adds its detailed rows (files copied, commit, notice path) to `oss/A1.md`, `oss/A2.md` and `oss/A3.md`.
- This index holds the decisions.
- Licences were read from each repository's `LICENSE` file where the GitHub API says `NOASSERTION`.

## Used

| Project | Licence (verified) | Activity (stars, last push) | Pinned | Use | Package |
|---|---|---|---|---|---|
| PostHog/posthog-js (`posthog-js`) | Apache-2.0 AND MIT (root LICENSE Apache-2.0; `packages/core`, `web`, `react-native` MIT) | 614, 2026-09-29 | npm 1.435.1, commit `666d10918246` | Browser SDK wrapped by `@railyard/analytics-js` (autocapture, recorder, heatmaps, surveys, flags) | A1 |
| `posthog-react-native` | MIT | same monorepo | npm 4.78.2 | Expo / React Native entry of `@railyard/analytics-js` (screens, touches, surveys) | A1 |
| `posthog-node` | MIT | same monorepo | npm 5.54.1 | Wrapped by `@railyard/analytics-node` | A1 |
| PostHog/posthog (outside `ee/` only) | MIT Expat (root LICENSE; `ee/` excluded; `products/product_analytics` and `products/experiments` carry no nested licence) | 40 001, 2026-09-29 | commit `cbb1e9253027` | Ported semantics and tests: `nodejs/src/common/persons/**` (A1), `products/product_analytics/backend/hogql_queries/{trends,funnels,retention,paths,stickiness,lifecycle}` (A2), `products/experiments/stats/**` (A3) | A1, A2, A3 |
| umami-software/umami (vendored as `apps/insights`) | MIT | 39 086, 2026-09-29 | v3.4.0, `ec0ff50388c2` | Postgres SQL for funnels, retention and journeys copied into `src/insights/ported/**` | A2 |
| rrweb-io/rrweb (`rrweb`, `rrweb-player`) | MIT | 20 225, 2026-09-29 | npm 2.1.6, commit `5b08843faf9c` | Replay playback page | A3 |
| honojs/hono (`hono`, `@hono/node-server`) | MIT | 32 386, 2026-09-29 | 4.13.11 / 2.1.3 | HTTP server | A1 |
| `pg` (node-postgres) | MIT | n/a | 8.23.0 | Postgres client | A1 |
| `fflate` | MIT | n/a | 0.8.3 | gzip decode of SDK batches | A1 |
| panva/jose | MIT | n/a | current | Console-to-service tokens | A1 |
| taozhi8833998/node-sql-parser | Apache-2.0 | 1 042, 2026-07-19 | 5.4.0 | SQL sandbox allowlist | A2 |
| mourner/simpleheat | BSD-2-Clause | 957, 2024-07-29 (small and finished) | 0.4.0 | Heatmap overlay | A3 |
| `@aws-sdk/client-s3` | Apache-2.0 | 3 670, 2026-09-29 | 3.1143.0 | S3 export destination | A3 |
| growthbook/growthbook `packages/stats` (gbstats) | MIT (`packages/stats/LICENSE`; enterprise dirs excluded) | 8 455, 2026-09-29 | commit `d09b71835230` | Cross-check vectors for the experiment stats port | A3 |
| `jstat` | MIT | n/a | 1.9.6 | Distributions, only if the stats port needs them | A3 |
| open-feature/js-sdk | Apache-2.0 | 283, 2026-09-29 | `a792d4dd8d22` | Already used by the flags slot's `@railyard/openfeature`; unchanged | n/a |

## Considered and not used (with the reason)

| Project | Licence | Why not |
|---|---|---|
| ClickHouse | Apache-2.0 | Box 2 has no measured room for it (see `CAPACITY.md`); Postgres with partitions and rollups first |
| `@duckdb/node-api` / duckdb | MIT | Recorded fallback for heavy queries over Parquet snapshots (PLAN section 6); not the default, because it adds a native binary and memory per query |
| FrigadeHQ/trench | MIT | Needs ClickHouse and Kafka; its event API shape is covered by the SDK protocol we speak |
| jitsucom/jitsu | MIT | An event pipeline (ingest to warehouses) rather than insights; our exports cover the need at our scale |
| highlight/highlight | Apache-2.0 outside `highlight.io/` and `enterprise/` | Replay stack with ClickHouse and Kafka; rrweb directly is smaller |
| sidequery/hogflare | **no licence** | Validates the "PostHog-protocol-compatible backend" pattern; no code copied |
| formbricks/formbricks | AGPL-3.0 core (SDK dirs MIT) | Surveys already render inside the SDKs we use; no AGPL service needed |
| openreplay/openreplay | ELv2 / AGPL parts (NOASSERTION) | Not OSI-permissive for embedding |
| Openpanel-dev/openpanel, aptabase/aptabase, plausible/analytics | AGPL-3.0 | AGPL; would only be allowed as an unmodified separate service, and they do not cover identify/cohorts/experiments together |
| matomo-org/matomo | GPL-3.0 | GPL |
| rudderlabs/rudder-server | ELv2 (NOASSERTION) | Not OSI |
| PostHog/posthog-js-lite, segmentio/analytics-node | MIT | Archived |
| MaxMind GeoLite2 | EULA (not OSI) | Country comes from the Cloudflare header instead |
