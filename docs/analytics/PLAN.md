# Railyard Product Analytics: plan (2026-09-30)

Branch `analytics/2026-09-30` in worktree `/Users/arhansubasi/products/railyard-analytics`, cut from railyard `main` at `4c51de5`.
Architect pass: this file, `LIMRUN-GAPS.md`, `OSS.md` and `CAPACITY.md`. Build packages: **A1**, **A2**, **A3** (section 9).

## 1. Goal

A Scribase customer runs `scribase add analytics` and gets event-level product analytics for their app:

- events, identify, alias and group analytics;
- autocapture on the web and screen capture in React Native;
- insights: trends, funnels (conversion windows, breakdowns), retention (cohort grid), paths, stickiness and lifecycle;
- cohorts (static and behavioural), dashboards, and SQL over the project's events;
- session replay with privacy masking, heatmaps and surveys;
- experiments on the existing flags slot, and exports (webhook, S3, CSV);
- per-project isolation, retention limits per plan, and GDPR delete and export.

Everything runs inside the box limits (`CAPACITY.md`).

## 2. What already exists (read on 2026-09-30, not edited)

| Thing | Where | What it gives us | Owner today |
|---|---|---|---|
| Insights (Umami v3.4.0 fork, MIT) | `RY/apps/insights/**` | Cookieless web analytics: visits, pages, UTM, goals, a Postgres funnel, retention and journey SQL, an rrweb recorder (`src/recorder`), heatmap tables, `session.distinct_id` linking | Wave P3 PY3.2/PY3.3 agent (`src/**`). **Read only for us.** |
| Insights edge collector | `RY/apps/insights-edge/**` | A Cloudflare Worker: bots dropped, daily salted visitor hash, caps at 100% (`src/caps.ts`), Rago meter `insights_events` | PY lane |
| Console | `RY/apps/console/**` | Next.js 14 on Workers (OpenNext). Sign-in, orgs, `railyard.apps` (`site_key` is the app's public key), entitlements, the Scribase bridge (`app/api/v1/bridge/scribase/*`), caps copy (`lib/caps.ts`) | PY lane |
| Flags slot (PY2.1) | branch `wave-p2-railyard`: `apps/console/lib/flags/**`, `app/api/v1/flags/client/route.ts`, `packages/openfeature-railyard/**`, migration `0005_flags.sql` | Flag envs per app environment, a public `ry_flags_...` client key, `{key, enabled, value}` flags, two backends (Flagsmith or table), a 15 s edge cache | PY lane. **Not on `main` yet** (see 9.4) |
| Scribase analytics slot | `SB/config/capabilities/analytics.json` (carve-out, PY lane) | `scribase add analytics` writes `NEXT_PUBLIC_ANALYTICS_SITE`, `EXPO_PUBLIC_ANALYTICS_SITE`, `VITE_ANALYTICS_SITE`, `ANALYTICS_SCRIPT_URL` and `ANALYTICS_COLLECTOR_URL` | PY lane (carve-out) |

### Why a second service and not more Umami

- Umami's model is website, session and visit. It has no person profiles, merges, groups or event-level person properties, and its privacy model is a daily-rotating visitor hash. That hash cannot back identify, retention by user, or cohorts of people.
- `apps/insights/src/**` belongs to another agent in this wave.
- So product analytics is a new service beside Insights, and **nothing in Insights is removed** (never-delete rule).
  - Insights stays the cookieless page analytics.
  - Product analytics is the identified, consent-gated event analytics.
  - The Scribase Analytics tab links to both.
- We still reuse Umami's Postgres SQL shapes (MIT, already vendored) as the starting point for funnels, retention and journeys (section 6).

## 3. The key decision: speak the PostHog capture protocol, reuse its MIT/Apache SDKs

**Decision.** `apps/product-analytics` implements the public wire protocol of the PostHog SDKs:

| Endpoint | Purpose |
|---|---|
| `/e/`, `/i/v0/e/`, `/batch/`, `/capture/` | Events |
| `/engage/` | Person properties |
| `/groups/` | Group properties |
| `/flags/?v=2` and `/decide/` | Flags, including legacy `decide` callers |
| `/array/:token/config` and `/array/:token/config.js` | Remote config |
| `/s/` | Replay snapshots |
| `/api/surveys/` | Surveys |

Accepted encodings:
- JSON;
- gzip (`compression=gzip-js`);
- base64 (`compression=base64`);
- form-encoded `data=`.

Result: the unmodified, maintained SDKs work against our host:
- `posthog-js` 1.435.1 (Apache-2.0 AND MIT);
- `posthog-react-native` 4.78.2 (MIT);
- `posthog-node` 5.54.1 (MIT);
- the iOS, Android, Flutter, Python and Go SDKs.

Those SDKs already give us autocapture, `$pageview`/`$pageleave`, `$screen`, identify, alias, groups, rrweb session recording with masking, heatmaps (`$$heatmap`), dead clicks, surveys rendering, feature-flag bootstrapping and the offline queue. We write none of that.

**Why this is the OSS-first answer:**
- It is the biggest block of maintained client code we can legally reuse.
- It works on every platform at once.
- The same pattern is proven in the field. `sidequery/hogflare` does it on Cloudflare. It has **no licence**, so it is a reference only and we copy no code.

**What we write** is glue:
1. ingestion into Postgres;
2. person, alias and group resolution (ported from PostHog `nodejs/src/common/persons/**`, MIT);
3. query builders (ported from Umami's Postgres SQL and PostHog `products/product_analytics/backend/hogql_queries/**`, MIT);
4. tenancy, caps and GDPR;
5. console screens.

**Public copy rule.** No competitor name appears in customer-visible HTML, docs or package names:
- Our packages are `@railyard/analytics-js` (re-exports `posthog-js` and `posthog-react-native` with our defaults) and `@railyard/analytics-node` (wraps `posthog-node`).
- Upstream names stay only in `NOTICE`, `LICENSE` and the dependency list.
- The copy lint (denylist `.3`) runs on every UI and doc file.

## 4. Architecture

```
app (web / Expo / server)
  @railyard/analytics-js | @railyard/analytics-node   (wrap the upstream MIT/Apache SDKs)
        |  HTTPS, public token = railyard.apps.site_key (the key `scribase add analytics` already writes)
        v
apps/product-analytics  (Node 22, Hono; one process; box 2; cap 192 MiB, see CAPACITY.md)
  capture  -> validate -> caps (byte-copy of console lib/caps.ts) -> person resolve -> batch insert (<= 2 s / 1000 rows)
  flags    -> reads the flags slot through its public client API (never its tables); A3 adds variants
  config   -> remote config: replay, heatmaps, surveys, autocapture switches per project
  query    -> insight builders, SQL sandbox, cohorts job, dashboards cache
  replay   -> snapshot chunks to S3-compatible storage (Scribase Storage S3 API or R2), index rows in Postgres
  jobs     -> in-process scheduler: rollups, cohort recompute, retention drops, GDPR deletes, exports
        |
        v
Postgres (a dedicated database `product_analytics` on the box 2 Postgres; a scratch initdb cluster in tests)
  events       range-partitioned by month (declarative), RLS by project
  persons, person_distinct_ids, person_overrides, groups
  rollups, cohorts, insights, dashboards, replays index, surveys, experiments, exports
console (Next.js on Workers) -> app/(app)/product-analytics/**  -> service API with a short-lived signed org token
Scribase tab                 -> existing bridge + proof ("first product event") through the service's bridge route
```

### 4.1 Tenancy and keys

- **Project.** One project per Railyard app. `project_id` is the `railyard.apps.id` UUID, and the public token is the app's `site_key`.
  - So `scribase add analytics` needs **no new key**. The SDK reads `NEXT_PUBLIC_ANALYTICS_SITE` (or the Expo and Vite equivalents) as its token.
- **Site directory.** The service resolves a token to `{org_id, app_id, events_limit, paused, retention}` through a `SiteDirectory` interface:
  - PostgREST implementation: reads `railyard.apps` and `railyard.entitlements` with the console project's service key, cached 60 s.
  - Memory implementation: for tests.
  - It is read-only. The console stays the writer.
- **Console to service.** The console signs a 5-minute HS256 token (`jose`, MIT) with `{org_id, app_ids, role, exp}` using `PRODUCT_ANALYTICS_CONSOLE_SECRET`. Every service query API verifies it and scopes to those app ids.
- **Isolation.** Every table has `project_id`, and RLS policies use `current_setting('pa.project_id')`.
  - The service sets it per transaction (`set local`).
  - Every query builder also takes `project_id` as a bound parameter.
  - That gives two independent guards, each with a cross-project test.
- **Host env.** Server: `PRODUCT_ANALYTICS_URL` (default `https://pa.railyard.ereboslab.com`). Public, in the app: `NEXT_PUBLIC_ANALYTICS_HOST`.
  - Adding the public name to `SB/config/capabilities/analytics.json` is a **hand-off line** for the PY steward (section 9.4). We do not edit Scribase.
  - Until it lands, the SDK default host is the constant above.

### 4.2 Consent and privacy

- **Consent.** When the consent slot (c15t, PY2.5) says "no":
  - `@railyard/analytics-js` starts in memory-only persistence with `opt_out_capturing_by_default`;
  - it switches to persistent identity only after consent;
  - nothing is sent before consent where the project turns "require consent" on (default on for EU visitors is a project setting, marked target).
- **IP addresses.** The service never stores an IP address.
  - Country comes from the `CF-IPCountry` header, because the host sits behind Cloudflare. No GeoIP database ships: GeoLite2 is under an EULA, not an OSI licence, so it is not added here.
  - `$ip` is dropped at ingest.
- **Replay masking defaults.** All inputs are masked. Text is masked under `[data-private]` and `.ph-mask`, and blocked under `[data-block]`. Canvas recording is off. Network payloads are not recorded.
  - Our wrapper sets these, and the project can only relax them in settings with a warning.

### 4.3 Storage model (Postgres 14+; tests on the local Homebrew Postgres 14.18)

| Table | Content | Notes |
|---|---|---|
| `pa.events` | `uuid, project_id, event, distinct_id, person_id, timestamp, properties jsonb, elements_chain text, session_id, window_id, "$group_0..4" text, inserted_at` | Partitioned by month on `timestamp`; indexes `(project_id, event, timestamp)`, `(project_id, person_id, timestamp)`, `(project_id, session_id)`, GIN on `properties` only if the measurement in A2 says it pays |
| `pa.persons` | `id, project_id, properties jsonb, is_identified, created_at, version` | Person-on-event: `person_id` written at ingest |
| `pa.person_distinct_ids` | `project_id, distinct_id, person_id, version` | |
| `pa.person_overrides` | `project_id, old_person_id, new_person_id` | Merges without rewriting history (PostHog's person-overrides approach, MIT); queries read `coalesce(o.new_person_id, e.person_id)` |
| `pa.groups` | `project_id, group_type_index, group_key, properties` | Up to 5 group types per project |
| `pa.rollup_hourly` | `project_id, event, hour, count` | Written by a job; trends read rollups for plain counts |
| `pa.ingest_receipts` | idempotency on event `uuid` | A retried batch is stored once (same rule as PY2.5) |

- Replay snapshots are gzip chunks in object storage. `pa.replays` holds the index: `session_id`, `person_id`, start and end, click and key counts, URLs and size.
- Retention works in two ways:
  - dropping whole monthly partitions older than the longest plan;
  - a nightly batched delete by project for shorter plans.

## 5. Features mapped to OSS and package

| Feature | Client side (reused) | Server side (ported or glue) | Package |
|---|---|---|---|
| Capture, batch, gzip and base64 | posthog-js / -react-native / -node | Protocol decode (`fflate`, MIT) + validation + caps + batch insert (`pg`, MIT) | A1 |
| Identify, alias, `$set`/`$set_once`/`$unset`, merge rules | SDKs | Port of PostHog `nodejs/src/common/persons/**` merge semantics (MIT), incl. "never merge two identified persons" | A1 |
| Groups (`$groupidentify`, `$groups`) | SDKs | group types, group properties | A1 |
| Web autocapture, `$pageview`, `$pageleave`, dead clicks | posthog-js (`autocapture.ts`) | store `elements_chain` | A1 |
| React Native screens, touches, app lifecycle | posthog-react-native `captureScreens` / `autocapture` | none | A1 |
| Remote config and plain flags | SDKs call `/array/:token/config` and `/flags` | Config assembled from per-feature providers; flags read through the flags slot's public client API | A1 (plain), A3 (variants, replay, surveys, heatmap sections) |
| Trends (count, unique people, DAU/WAU/MAU, sum/avg/min/max/p50-p99, breakdown, formula, compare to previous) | none | SQL builders ported from PostHog trends runner semantics and Umami `getEventMetrics`-style Postgres SQL | A2 |
| Funnels (ordered, strict, unordered; conversion window; breakdown; time to convert; drop-off people) | none | Port Umami `queries/sql/funnels/getFunnel.ts` (window-function funnel on Postgres, MIT) + PostHog funnel semantics | A2 |
| Retention grid (first-time or recurring; day, week, month) | none | Port Umami `retention/getRetention.ts` + PostHog retention semantics | A2 |
| Paths (from/to event, top N edges, depth) | none | Port Umami `journeys/getJourney.ts` + PostHog paths semantics | A2 |
| Stickiness, lifecycle (new, returning, resurrecting, dormant) | none | Port PostHog `stickiness`/`lifecycle` runner semantics | A2 |
| Actions (named events incl. autocapture selectors) | none | Port PostHog `elements_chain` matching (MIT) | A2 |
| Cohorts (static via CSV or list; behavioural "did X n times in d days", property filters) | none | Nightly and on-demand recompute into `pa.cohort_people` | A2 |
| Dashboards, saved insights, result cache | none | tables + cache keyed by (params hash, data version) | A2 |
| SQL over events | none | Read-only role + RLS + `node-sql-parser` (Apache-2.0) allowlist (single SELECT, `pa_view.*` only), `statement_timeout` 10 s, 10 000 rows | A2 |
| Session replay | posthog-js recorder (rrweb, MIT) | `/s/` ingest, chunk store, index; playback page served by the service with `rrweb-player` 2.1.6 (MIT), embedded by the console | A3 |
| Heatmaps, scroll depth | posthog-js heatmaps (`$$heatmap`) | aggregate per URL; overlay with `simpleheat` (BSD-2) on an iframe of the page | A3 |
| Surveys (popover, API, targeting, results) | posthog-js / -react-native surveys rendering | `/api/surveys/` + CRUD + results from `survey sent` events | A3 |
| Experiments | SDK flag variants + `$feature_flag_called` | Variant rollout stored in the flag's own `value` (JSON) through the flags slot's service; assignment hash compatible with the SDKs' local evaluation; stats ported from PostHog `products/experiments/stats/{bayesian,frequentist}` (MIT) with their test vectors | A3 |
| Exports | none | Webhook (Standard Webhooks, the console's existing signer pattern), S3 NDJSON.gz or CSV (`@aws-sdk/client-s3`, Apache-2.0), CSV download; cursor per destination, retry with backoff | A3 |
| GDPR delete and export | none | Delete person (events, persons, ids, replays, heatmaps, survey answers) within 24 h, with a visible job; export JSON per person | A1 |
| Retention limits per plan | none | Partition drop + nightly project delete; replay TTL | A1 (events), A3 (replays) |

## 6. Query layer rules (A2)

- **Builders.** Every insight is a pure function `(query, projectId) -> {sql, params}` plus a runner.
  - No string interpolation of user input. Property keys are bound as parameters (`properties ->> $n`).
- **Golden tests.** Each builder has golden fixtures: a generated event set with known answers, checked against a hand-written SQL count and against the ported upstream test cases where those exist (PostHog `test_*` cases for funnels, retention, stickiness and lifecycle, re-expressed as fixtures).
- **Latency budget (targets, measured by A2 on the Mac with 1 000 000 generated events in one project):**
  - trends, 30 days: p95 under 1.5 s;
  - funnel, 3 steps, 14-day window: under 3 s;
  - retention, 8 weeks: under 3 s.
  - If a budget fails, first add the rollup or index the measurement points to. DuckDB (`@duckdb/node-api`, MIT) over monthly Parquet snapshots is the recorded fallback, not the default.
- **Charts.** No new console dependency. The console package manifest is not ours; its charts are small SVG components in `app/(app)/product-analytics/_components/**`, following the dataviz rules:
  - line and bar for trends;
  - bars with drop-off for funnels;
  - an HTML table heat grid for retention;
  - a ranked edge list with ribbons for paths.

## 7. Flags and experiments contract (A3)

- **Flag writes.** We never write the flag store directly.
  - The experiment screen in the console calls the flags slot's own server functions (`lib/flags/service.ts` `setFlag`) because it runs in the same console app.
  - The service reads flags only through `GET /api/v1/flags/client` with the environment's public `ry_flags_` key.
- **Experiment flag value.** An experiment is `(project, flag env id, flag key, variants, metrics, status)`. Starting it sets the flag's `value` to:
  `{"rollout":100,"variants":[{"key":"control","weight":50},{"key":"test","weight":50}],"experiment":"<id>"}`
- **Variant assignment.** The service's `/flags` answer evaluates that value per `distinct_id` with the SDKs' documented hashing (sha1 of `"<flag>.<distinct_id>"`, first 15 hex digits over `0xfffffffffffffff`). The same person gets the same variant in the SDK, the server and the stats.
- **OpenFeature.** `@railyard/openfeature` keeps reading `enabled` and `value` unchanged. Old clients see the JSON value, which is documented.
- **Exposure and stats.** Exposure is `$feature_flag_called`. Results come from the ported stats (Bayesian chance-to-win and frequentist p-value / CI), with a sample-ratio-mismatch check.

## 8. Plans, caps and retention (targets, marked as such until the owner sets them)

- **Where limits come from.** All limits live in `apps/product-analytics/src/plans.ts` with a `copy-source` marker. The console reads them from the service; nothing is hard-coded in copy.
- **Starting targets:**
  - events share the Railyard plan's `eventsLimit` (console `lib/plans.ts`, read only);
  - events are kept 12 months on the add-on;
  - replays are kept 30 days, with a 10% default sample and 30-minute maximum recordings;
  - surveys: 5 active;
  - exports: 3 destinations.
- **At 100% of the cap** the service answers the SDK with a normal 200 (SDKs retry otherwise), counts and does not store, and the console says exactly that. It reuses the `capMessage` wording by byte-copy of `lib/caps.ts` with a drift test, as the edge does.
- **Metering.** Stored and capped counts go to Rago through the section 5.3 consumer contract (meter `scribase.analytics.events`, `transaction_id = "scribase:analytics.events:<org>:<minute>:pa"`), behind `RAILYARD_LIVE`.

## 9. Packages

### 9.1 A1: ingest, storage, SDKs, identify and groups, GDPR

**Owns (new files only):**
- `RY/apps/product-analytics/**` **except** the folders A2 and A3 own below. It creates:
  - `package.json`;
  - its own `pnpm-lock.yaml` (standalone, like `apps/insights`);
  - `NOTICE` and `README.md`;
  - `src/server.ts` and `src/routes.ts`, with the mount lines for `./insights/routes.ts`, `./replay/routes.ts`, `./heatmaps/routes.ts`, `./surveys/routes.ts`, `./experiments/routes.ts` and `./exports/routes.ts`. Each is created as an empty `Hono` router and handed over in the commit message.
  - `src/remote-config/sections.ts`, the provider registry, with empty `replay`, `heatmaps` and `surveys` providers handed to A3;
  - `src/flags/variants.ts`, a skeleton returning `null`, handed to A3;
  - `src/capture/**`, `src/persons/**`, `src/groups/**`, `src/tenancy/**`, `src/caps.ts`, `src/plans.ts`, `src/gdpr/**`, `src/jobs/**` (scheduler and retention drop);
  - `db/migrations/0001-0009`;
  - `deploy/compose.fragment.yml` (rule 0.7);
  - `test/a1/**`.
- `RY/packages/analytics-js/**`, which includes the `react-native` entry.
- `RY/packages/analytics-node/**`.
- `RY/apps/console/app/(app)/product-analytics/{page.tsx,_lib/http.ts,_lib/token.ts,persons/**,setup/**,privacy/**}` and `_components/Shell.tsx`.
- The **one nav line** in `RY/apps/console/components/Nav.tsx` (`{ href: "/product-analytics", label: "Product analytics" }`).
- `RY/docs/analytics/oss/A1.md`.

**OSS:**
- SDKs: `posthog-js` 1.435.1, `posthog-react-native` 4.78.2, `posthog-node` 5.54.1.
- Server libraries: `hono` 4.13.11 and `@hono/node-server`, `pg` 8.23.0, `fflate` 0.8.3, `jose`.
- Porting source: PostHog `nodejs/src/common/persons/**` (MIT, commit `cbb1e9253027`).

**Acceptance (tests first, scratch Postgres via `initdb` as in `apps/console/test/migration.test.ts`):**
1. **Protocol:** the unmodified `posthog-node` and `posthog-js` point at the local service and store events through every endpoint and encoding listed in section 3, with the right fields.
   - posthog-js runs in jsdom, plus one Playwright Chromium run; Chromium 1243 is already in the cache, so nothing is installed.
   - A malformed or oversized payload gets a plain 400 or 413 and is never partly stored.
2. **Identify:** anonymous events followed by `identify` resolve to one person, and the earlier events count for that person in queries through overrides.
   - Alias works.
   - Merging two identified persons is refused, as upstream does.
   - `$set_once` keeps the first value.
   - Group properties and the `$groups` fields land.
3. **Isolation:** app A's token and console token can never read or write app B, at the API and at the SQL/RLS level (two separate tests).
4. **Caps and pause:** at 100% the SDK gets 200, the event is counted and not stored, and the console shows the `capMessage` text. A paused org stores nothing. `caps.ts` is a byte-copy of the console file, with a drift test.
5. **Retries and privacy:** a retried batch (same event uuids) is stored once. No IP address is in any table or log (grep test over a captured run).
6. **GDPR:**
   - `POST /v1/projects/:id/persons/:distinct_id/delete` removes the person's events, ids, person row and (via a hook A3 fills) replays and survey answers within 24 h. The job is visible in the console and there is a test with the clock moved.
   - Export returns one JSON file with the person, their events and the replay list.
7. **Retention:** partitions older than the plan are dropped, and shorter plans are trimmed per project, both on a moved clock. Nothing inside the retention window is ever removed.
8. **SDK wrappers:**
   - `@railyard/analytics-js` reads `NEXT_PUBLIC_ANALYTICS_SITE` / `EXPO_PUBLIC_ANALYTICS_SITE` / `VITE_ANALYTICS_SITE` and the host.
   - It sets the privacy defaults from 4.2 and waits for consent when the project requires it; an e2e network check shows no request before consent.
   - The Expo example sends `$screen` on navigation, and an app that was offline for 10 minutes flushes every event exactly once.
   - No upstream name appears in our README, types or console copy (denylist `.3` lint).
9. **Scribase proof:** `POST /v1/bridge/scribase/proof` (Standard Webhooks, the same secret name as the console bridge) returns `{slot:"analytics", live, proof:{kind:"first_product_event", id, at}}`.
   - The console page shows the first event within 60 s of the example app starting.
10. **Capacity:** the service's RSS stays within 192 MiB at 50 events/s for 10 minutes on the Mac. The measurement goes in `CAPACITY.md` (the A1 row).

### 9.2 A2: insights in the console

**Owns:**
- `RY/apps/product-analytics/src/insights/**` (trends, funnels, retention, paths, stickiness, lifecycle, actions, cohorts, dashboards, SQL sandbox, result cache, rollup job);
- `db/migrations/0010-0019`;
- `test/a2/**`;
- `RY/apps/console/app/(app)/product-analytics/{insights,dashboards,cohorts,sql,actions}/**`, `_lib/insights-client.ts` and the chart components `_components/charts/**`;
- `RY/docs/analytics/oss/A2.md`.

**OSS:**
- Umami v3.4.0 Postgres SQL for funnels, retention and journeys. It is read from `apps/insights/src/queries/sql/**` and **copied** into `src/insights/ported/**` with its MIT notice. The source is never edited.
- PostHog `products/product_analytics/backend/hogql_queries/{trends,funnels,retention,paths,stickiness,lifecycle}` semantics and test cases (MIT, commit `cbb1e9253027`).
- `node-sql-parser` 5.4.0 (Apache-2.0).

**Acceptance:**
1. **Hand SQL:** every insight type matches a hand-written SQL answer on the shared fixture set. That covers:
   - trends with breakdowns, math and formula;
   - funnels: ordered, strict and unordered, with the conversion window, breakdown, time to convert and the drop-off people list;
   - retention, first-time and recurring, on the day, week and month grids;
   - paths with from/to and depth;
   - stickiness;
   - lifecycle's four states.
2. **Cohorts:**
   - A behavioural cohort ("did `signup` at least 2 times in 30 days" plus a person property) matches a hand SQL count on fixtures.
   - A static cohort imports from CSV.
   - Cohorts filter every insight type.
3. **Dashboards:** tiles hold saved insights, a refresh uses the cache, and a stale cache is labelled with its computed-at time.
4. **SQL sandbox:**
   - Only a single SELECT over `pa_view.events`, `pa_view.persons`, `pa_view.groups` and `pa_view.sessions` is accepted.
   - `DELETE`, multiple statements, `pg_*` functions and other schemas are refused with a plain message.
   - RLS still holds if the parser were bypassed (test calls the runner with a hostile string).
   - Timeout 10 s, 10 000 rows, CSV download.
5. **Latency:** section 6 budgets measured and written to `CAPACITY.md`.
6. **Console:** screens are one action per screen, have no emoji, pass the copy lint, render the SVG charts, and use no new console dependency.

### 9.3 A3: session replay, heatmaps, surveys, experiments on the flags slot, exports

**Owns:**
- `RY/apps/product-analytics/src/{replay,heatmaps,surveys,experiments,exports}/**`;
- `src/remote-config/sections.ts` (handed over by A1: "then");
- `src/flags/variants.ts` (handed over by A1: "then");
- `db/migrations/0020-0029`;
- `test/a3/**`;
- the service's `public/player/**` (the built rrweb-player page);
- `RY/apps/console/app/(app)/product-analytics/{replay,heatmaps,surveys,experiments,exports}/**` and `_lib/a3-client.ts`;
- `RY/docs/analytics/oss/A3.md`.

**OSS:**
- `rrweb` and `rrweb-player` 2.1.6 (MIT);
- `simpleheat` 0.4.0 (BSD-2-Clause);
- `@aws-sdk/client-s3` (Apache-2.0);
- PostHog `products/experiments/stats/**` (MIT), ported to TypeScript with its test vectors. GrowthBook `packages/stats` (MIT) is the cross-check;
- `jstat` (MIT) for distributions, if the port needs it.

**Acceptance:**
1. **Replay:**
   - A posthog-js session recorded in Playwright Chromium plays back in the service's player page, embedded in the console with a signed 10-minute URL. Inputs are masked in the stored chunks (byte check: the typed test string is absent).
   - A replay opens from a person, an event or a funnel drop-off.
   - Replays expire after the plan's days.
   - The GDPR hook deletes them.
2. **Heatmaps:** clicks and scroll depth for a URL aggregate correctly on fixtures and render as an overlay on the page iframe.
3. **Surveys:**
   - A survey created in the console appears in the example web app and the Expo app, targeted by URL, flag or cohort.
   - Answers land as events and the results screen matches a hand count.
4. **Experiments:**
   - Starting an experiment sets the flag value only through the flags slot's service function.
   - `/flags` returns the same variant as the SDKs' local hashing for 10 000 distinct ids.
   - The stats match the upstream test vectors within 1e-6.
   - A 60/40 split on a 50/50 experiment raises the sample-ratio warning.
5. **Exports:**
   - A webhook destination receives signed batches, and a 500 is retried with backoff.
   - An S3 destination (tested against a local S3-compatible fake: `s3rver` MIT or an in-memory mock) receives NDJSON.gz.
   - A CSV download for an insight or SQL result works.
   - A destination never receives another project's rows.
6. **Capacity:** replay ingest and export jobs keep the service within its 192 MiB cap. Replay bytes per minute are measured on the fixture and written to `CAPACITY.md`.

### 9.4 Order, prerequisites and hand-offs

**Order.** A1 first. A2 and A3 run in parallel after A1 commits, because their files are disjoint. All work is local commits on `analytics/2026-09-30`.

**Before A3 starts:**
- The flags slot (`wave-p2-railyard`, PY2.1) must be on `main`, and this branch must have merged `main` again.
- The orchestrator does that merge (owner rule 2026-09-30: everything lands on main).
- If it has not landed, A3 builds experiments against a fixture of the flags client API and `setFlag` signature copied from `wave-p2-railyard:apps/console/lib/flags/{types,service}.ts`. It records "flags merge pending" in its commit message.

**Hand-off lines for the PY steward (not done by these packages; outside the allowed paths):**
1. `SB/config/capabilities/analytics.json`: add `NEXT_PUBLIC_ANALYTICS_HOST`, `EXPO_PUBLIC_ANALYTICS_HOST` and `VITE_ANALYTICS_HOST`, and the second proof `first_product_event`.
2. `RY/apps/console/app/api/v1/bridge/scribase/service.ts`: one `case "product_analytics"` forwarding to the service's proof route.
3. `RY/pnpm-workspace.yaml`: optional workspace lines for the three new packages. Each is standalone until then.
4. `IB/stacks/product-analytics/**`: wrap A1's compose fragment with the cap from `CAPACITY.md`, marked "not deployed".
5. The `RY/docs/OSS_FOUNDATIONS.md` index row pointing to `docs/analytics/OSS.md`.

### 9.5 Rules every package follows

- **Safety:**
  - Tests are written first.
  - No stubs in delivered flows. The only allowed empties are the skeletons A1 hands to A3 and A2, which are named above.
  - Nothing touches production: no ssh, deploys, prod DB writes, live payments or `git push`.
  - No docker builds or prune.
  - Run `df -h /` before any install and stay above 10 GB free (14 GiB free at plan time).
- **Scope:** A package edits no path outside its list. In particular, never edit `RY/apps/insights/src/**`, `RY/packages/insights-*/**` or `RY/apps/console/app/(app)/insights/**`.
- **Copy:** no emoji, no "indie" wording, no competitor names in public copy, and no unsourced numbers in public copy. Internal targets are marked "target".
- **Licences:** MIT, Apache or BSD only, with `NOTICE` rows. No GPL or AGPL anywhere in the SDK packages.
- **Commits:** local only, ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
