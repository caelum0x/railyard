# Mixpanel parity checklist

Written 2026-10-15 against the code on main, not against the plan file's own
claims. Every row below cites a file or a test that was actually read or run
in this pass; `node scripts/parity/recount.mjs` (A-rows) and
`pnpm run test:all` / `pnpm test test/qy/mixpanel-compat.test.ts` in
`apps/product-analytics` are the evidence for most of it. This service's own
wire protocol is PostHog's (`/batch/`, `/capture/`, `/flags/`); it is not, and
has never claimed to be, a drop-in Mixpanel endpoint. This checklist asks a
narrower, honest question: for a team migrating off Mixpanel, which parts
already work, which are feature-equivalent but need a different client, and
which are missing.

## Ingestion compat (`/track`, `/engage`, `/groups`, `/import`)

Checked with the real, unmodified `mixpanel` npm package (official Node SDK,
MIT) in `apps/product-analytics/test/qy/mixpanel-compat.test.ts`, pointed at
this service over plain HTTP -- not a mock, not a reimplementation of the
client.

| Row | Status | Evidence |
|---|---|---|
| `/track` (event capture) | **DONE** | `mixpanel.track()` lands through the shared capture path: event name, `distinct_id`, custom properties and the project token are all read from `properties.*` the same way for Mixpanel's and PostHog's shapes (`src/capture/normalize.ts`). Proven in `mixpanel-compat.test.ts`. |
| `/engage` -- `$set` / `$set_once` / `$unset` | **DONE** | `mixpanel.people.set/set_once/unset()` land through `/engage` (`src/capture/routes.ts`): the handler already reads `$distinct_id`, `$token` and exactly these three ops. Proven in `mixpanel-compat.test.ts`. |
| `/engage` -- `$add` / `$append` / `$union` / `$remove` | **MISSING** | `mixpanel.people.increment/append/union/remove()` are accepted (200) and silently produce no change: `/engage`'s handler only implements `$set`, `$set_once` and `$unset` (`src/capture/routes.ts:102-107`). Proven (and the silent-drop made visible) in `mixpanel-compat.test.ts`. |
| `/groups` | **MISSING** | The real SDK's `$group_key` means the group *type* (e.g. `"company"`) and `$group_id` means the group *key* (e.g. `"acme"`); this service's `/groups` handler reads `$group_type` for the type and `$group_key` for the key -- the opposite meaning for `$group_key`, and a field (`$group_id`) it never reads. A real `mixpanel.groups.set()` call is refused with 400 (`"group_type is required"`), not silently dropped. Proven in `mixpanel-compat.test.ts`. Fixing this is a routing change (disambiguate by the presence of `$group_id`) left to a follow-up task, not attempted here to avoid touching production ingestion routing and its A1 guarantees in a docs/recount pass. |
| `/import` (historical bulk import, separate auth) | **MISSING** | No `/import` route exists (`grep -rn "/import" src/` is empty); only the live paths map to the shared capture handler (`src/capture/routes.ts:64`). |
| Default (non-verbose) response contract | **MISSING** | A real Mixpanel client without `verbose: true` treats any response that is not the literal text `"1"` as a failure; this service always answers PostHog-shaped JSON (`{"status":1}`). Every call in `mixpanel-compat.test.ts` uses `verbose: true` to see past this. Without it, every SDK call against this service reports an error today, `/track` and `/engage` included. |

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
| Group analytics | DONE | `src/groups/` (A21); note the `/groups` *ingestion* gap above still applies to a real Mixpanel client. |
| Lookup tables | **MISSING** | No code: `grep -rli "lookup.table\|lookup_table" src` is empty. |
| Data views | **MISSING** | No code: `grep -rli "data.view\|data_view" src` is empty. |

## SDK coverage (A4, A3, L6)

| Row | Status | Evidence |
|---|---|---|
| Browser / Node / React Native (PostHog-protocol) | DONE | `packages/analytics-js`, `packages/analytics-node`; `test/a1/sdk-js.test.ts`, `test/a1/sdk-node.test.ts` (A3). |
| Native iOS/macOS/tvOS/watchOS (Swift), Android/JVM (Kotlin), Flutter/Dart | DONE in code, verified in part | `packages/analytics-swift` (tests run here: `swift test`, 6/6 green), `packages/analytics-kotlin`, `packages/analytics-flutter` (own test suites present, not re-run in this environment; no kotlinc/flutter toolchain here). No sample/example app end-to-end proof yet -- see `scripts/parity/wave-r.mjs` (A4). |
| Python, Go, Java SDKs | PARTIAL | Upstream-compatible clients exist per `scripts/parity/rows.mjs` L6; no published compat result for those three (this checklist's Node-SDK result is the first one on the record). |

## Net read

For a team moving off Mixpanel: event tracking and simple user-property sets
(`set`/`set_once`/`unset`) work today with the official Mixpanel SDK pointed
at this host and `verbose: true`. Group profiles, the increment/append/union/
remove people ops, `/import`, lookup tables and data views do not, and the
plain (non-verbose) response contract doesn't either. The underlying
analysis engine (funnels, retention, flows/paths, cohorts, SQL) is at parity
and already proven against the service's own PostHog-shaped SDKs; it is not
gated on closing the Mixpanel ingestion gaps above.
