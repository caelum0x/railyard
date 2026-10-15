# Mixpanel parity checklist

Written 2026-10-15 against the code on main, not against the plan file's own
claims. Updated the same day to close the five rows the first pass left
open: `/groups` field disambiguation, `$add`/`$append`/`$union`/`$remove`,
`/import`, lookup tables and data views. Every row below cites a file or a
test that was actually read or run in this pass; `node
scripts/parity/recount.mjs` (A-rows) and `pnpm run test:all` /
`pnpm test test/qy/*.test.ts` in `apps/product-analytics` are the evidence
for most of it (225 tests, 221 pass, 0 fail, 4 skipped -- no cached
Chromium; unrelated -- after this update, up from 220/216/0/4 before it).
This service's own wire protocol is PostHog's (`/batch/`, `/capture/`,
`/flags/`); it is not, and has never claimed to be, a drop-in Mixpanel
endpoint. This checklist asks a narrower, honest question: for a team
migrating off Mixpanel, which parts already work, which are
feature-equivalent but need a different client, and which are missing.

## Ingestion compat (`/track`, `/engage`, `/groups`, `/import`)

Checked with the real, unmodified `mixpanel` npm package (official Node SDK,
MIT) in `apps/product-analytics/test/qy/mixpanel-compat.test.ts`, pointed at
this service over plain HTTP -- not a mock, not a reimplementation of the
client.

| Row | Status | Evidence |
|---|---|---|
| `/track` (event capture) | **DONE** | `mixpanel.track()` lands through the shared capture path: event name, `distinct_id`, custom properties and the project token are all read from `properties.*` the same way for Mixpanel's and PostHog's shapes (`src/capture/normalize.ts`). Proven in `mixpanel-compat.test.ts`. |
| `/engage` -- `$set` / `$set_once` / `$unset` | **DONE** | `mixpanel.people.set/set_once/unset()` land through `/engage` (`src/capture/routes.ts`): the handler already reads `$distinct_id`, `$token` and exactly these three ops. Proven in `mixpanel-compat.test.ts`. |
| `/engage` -- `$add` / `$append` / `$union` / `$remove` | **DONE** (2026-10-15) | `PropertyOps` (`src/persons/properties.ts`) grew `add`/`append`/`union`/`remove` fields and `applyOps` applies them (numeric increment, list push, de-duplicated list merge, scalar removal) the same way it already applied `set`/`set_once`/`unset`; `/engage` (`src/capture/routes.ts`) now reads `$add`/`$append`/`$union`/`$remove` off each update into `eventOps`, the exact function `$set` already went through. Proven with `mixpanel.people.increment/append/union/remove()` in `mixpanel-compat.test.ts`. |
| `/groups` | **DONE** (2026-10-15) | `/groups` (`src/capture/routes.ts`) now disambiguates by the presence of `$group_id` (a field only the Mixpanel wire shape ever sends): when present, `$group_key` is read as the group TYPE and `$group_id` as the KEY (Mixpanel's meaning); otherwise `$group_type`/`$group_key` keep this service's own meaning (unchanged, still covered by `test/a1/identity.test.ts`). `upsertGroup` (`src/groups/groups.ts`) now applies a full `PropertyOps` (`$set`/`$set_once`/`$unset`/`$union`/`$remove`) the same way person profiles do, instead of a flat jsonb merge. Proven with `mixpanel.groups.set/union/remove()` in `mixpanel-compat.test.ts`. |
| `/import` (historical bulk import, separate auth) | **DONE** (2026-10-15) | New `/import`/`/import/` route (`src/capture/routes.ts`) shares `normalizeEvents` and `CapturePipeline.ingest` with `/track`, differing only where the real API differs: (1) auth -- requires Basic authentication or an explicit `api_key`/`token` query parameter, refusing a token that only appears in the event body (what `/track` accepts); (2) retention -- passes `{ historical: true }` so `CapturePipeline.ingestNow` (`src/capture/pipeline.ts`) skips the retention-window cutoff that silently drops old events on `/track`; (3) idempotency -- shared with `/track` via the existing uuid/`$insert_id` dedup (`pa.ingest_receipts`, `src/capture/writer.ts`). `properties.time` (Mixpanel's own event-time field, distinct from this service's `$time`) is now also read by `src/capture/normalize.ts`'s `eventTime`. Proven with `mixpanel.import()` (a 400-day-old event lands via `/import`, is dropped via `/track`, and re-importing the same `$insert_id` stores it once) and a direct-fetch 401 for the no-credential case, in `mixpanel-compat.test.ts`. |
| Default (non-verbose) response contract | **MISSING** | A real Mixpanel client without `verbose: true` treats any response that is not the literal text `"1"` as a failure; this service always answers PostHog-shaped JSON (`{"status":1}`). Every call in `mixpanel-compat.test.ts` uses `verbose: true` to see past this. Without it, every SDK call against this service reports an error today, `/track` and `/engage` included. Not attempted in this pass (out of the five rows scoped here); would need every capture route to detect the caller's `verbose` query param and switch its response body, which touches `/track`'s and `/engage`'s existing A1-proven response contract. |

## Analysis parity (Insights, Funnels, Flows, Retention)

These are feature rows, not wire-protocol rows: the service's own console and
SDKs (`@railyard/analytics-js`, `@railyard/analytics-node`, and now the native
Swift/Kotlin/Flutter wrappers) reach them; a Mixpanel client's *analysis* API
calls (`/query`, segmentation, etc.) are not implemented, only ingestion is
checked above.

| Row | Status | Evidence |
|---|---|---|
| Insights (trends, breakdowns, formulas) | DONE | `apps/product-analytics/test/a2/trends.test.ts` (A5); `scripts/parity/rows.mjs` A5. |
| Funnels (ordered, strict, unordered, breakdown, drop-off) | DONE | `apps/product-analytics/test/a2/funnels.test.ts` (A6). |
| Flows (paths) | DONE | `apps/product-analytics/test/a2/grids.test.ts` (A8, "Paths"): sessions split at 30 minutes, repeats collapse. |
| Retention | DONE | `apps/product-analytics/test/a2/grids.test.ts` (A7): day/week/month grids, first-time and recurring. |
| Cohorts (behavioural and static/CSV) | DONE | `apps/product-analytics/test/a2/actions-cohorts.test.ts` (A11). |
| Dashboards / saved insights | DONE | `apps/product-analytics/test/a2/dashboards.test.ts` (A12). |
| SQL over events | DONE | `apps/product-analytics/test/a2/sandbox.test.ts` (A13). |
| Group analytics | DONE | `src/groups/` (A21); the `/groups` *ingestion* row above is now also closed. |
| Lookup tables | **DONE** (2026-10-15) | New `pa.lookup_tables`/`pa.lookup_table_rows` (`db/migrations/0031_lookup_tables_data_views.sql`) hold a CSV the project owner uploads (`src/insights/lookup-tables.ts`, reusing `cohorts.ts`'s existing CSV reader), keyed by its first column. A new `"lookup"` `PropertyFilter` (`src/insights/types.ts`) joins a table's rows by an event/person/group property and compares one of its columns through `filters.ts`'s existing `compareProperty` -- the same comparison code every other filter type already uses, via a new `Joins.lookup()` that follows the exact `left join ... on key = (... ->> ...)` pattern the "group" filter type already established. CRUD is wired into the console API at `/v1/projects/:id/insights/lookup-tables`. Proven end to end (CSV upload, a trends query filtered and re-filtered by a lookup column, CSV replace) in `test/qy/lookup-tables-data-views.test.ts`. Not attempted: breakdown-by-lookup-column (only filtering is wired; `Breakdown` in `types.ts` is unchanged). |
| Data views | **DONE** (2026-10-15) | New `pa.data_views` (same migration) holds a named, reusable `PropertyFilter[]` (`src/insights/store.ts`'s `createDataView`/`updateDataView`/`listDataViews`/`deleteDataView`, the same CRUD shape as `createSavedInsight`). A query names one by `data_view_id` (new field on every `InsightQuery`, `src/insights/types.ts`); `runner.ts`'s `applyDataView` resolves it to a plain `properties` filter list -- merged ahead of the query's own filters -- before `runInsight`/`runFunnelPeople`/`runTimeToConvert` call any builder, so trends.ts/funnels.ts/etc. need no changes and see an ordinary query. CRUD is wired into the console API at `/v1/projects/:id/insights/data-views`. Proven (a trends query scoped by a data view, AND'd with the query's own filter, and a 400 for an unknown id) in `test/qy/lookup-tables-data-views.test.ts`. |

## SDK coverage (A4, A3, L6)

| Row | Status | Evidence |
|---|---|---|
| Browser / Node / React Native (PostHog-protocol) | DONE | `packages/analytics-js`, `packages/analytics-node`; `test/a1/sdk-js.test.ts`, `test/a1/sdk-node.test.ts` (A3). |
| Native iOS/macOS/tvOS/watchOS (Swift), Android/JVM (Kotlin), Flutter/Dart | DONE in code, verified in part | `packages/analytics-swift` (tests run here: `swift test`, 6/6 green), `packages/analytics-kotlin`, `packages/analytics-flutter` (own test suites present, not re-run in this environment; no kotlinc/flutter toolchain here). No sample/example app end-to-end proof yet -- see `scripts/parity/wave-r.mjs` (A4). |
| Python, Go, Java SDKs | PARTIAL | Upstream-compatible clients exist per `scripts/parity/rows.mjs` L6; no published compat result for those three (this checklist's Node-SDK result is the first one on the record). |

## Net read

For a team moving off Mixpanel: event tracking, user-property sets and the
increment/append/union/remove ops, group profiles (via `/groups`), historical
import (via `/import`), lookup tables and data views all now work with the
official Mixpanel SDK (`verbose: true`) or this service's own console API.
The one remaining ingestion gap is the plain (non-verbose) `"1"` response
contract -- every SDK call still needs `verbose: true` to see past this
service's JSON response body. The underlying analysis engine (funnels,
retention, flows/paths, cohorts, SQL) is at parity and already proven
against the service's own PostHog-shaped SDKs; none of the five rows closed
today required a change to that engine.
