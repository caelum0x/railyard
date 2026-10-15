# Railyard product analytics service

Event analytics for the apps of a Railyard org: events, people, identify and
alias, groups, privacy deletion and export, and the console API. Apps send
with `@railyard/analytics-js` (web and Expo) or `@railyard/analytics-node`
(servers). The token is the app's public site key, so `scribase add analytics`
needs no new key.

Status: not deployed. Limits are targets until the owner sets them
(`src/plans.ts`).

## Run

```sh
DATABASE_URL=postgres://... node scripts/migrate.ts
DATABASE_URL=postgres://... SITE_DIRECTORY_URL=... SITE_DIRECTORY_SERVICE_KEY=... node src/server.ts
```

The database user in `DATABASE_URL` owns the schema (it runs the migrations)
and is a member of `pa_app`; every request runs as `pa_app`, scoped to one
app by row level security.

Settings (environment):

| Name | What |
|---|---|
| `DATABASE_URL` | The service database; migrations run as its owner |
| `SITE_DIRECTORY_URL`, `SITE_DIRECTORY_SERVICE_KEY` | The console project's REST base and key, used read only for apps, entitlements and bindings |
| `PRODUCT_ANALYTICS_CONSOLE_SECRET` | Shared with the console, which signs 5-minute org tokens with it |
| `RAILYARD_SCRIBASE_BRIDGE_SECRET` | The bridge secret, the same as the console's |
| `FLAGS_URL` | The console base whose public flags client API is read |
| `RAGO_URL`, `RAGO_API_KEY`, `RAILYARD_LIVE` | Usage metering; only `RAILYARD_LIVE=1` sends |
| `PA_EXPORTS_KEY` | 32 bytes (hex or base64) that seal export destination secrets at rest |
| `PA_PUBLIC_URL`, `PA_CONSOLE_URL` | Where links in emails point: unsubscribe (this service) and "open" (the console) |
| `RALLY_TX_URL`, `RALLY_TX_KEY` | Alert and weekly report email through Rally Send (https only); without them each email is logged as not configured |

## What happens to events

- Under the plan's monthly limit every event is stored.
- At the limit the app still gets a normal answer; events are counted and not
  stored until the next UTC month. Nothing stored is removed.
- A paused org stores nothing.
- No IP address is stored or logged. The country comes from the edge's
  country header.
- A retried batch is stored once (event ids are remembered for 35 days).
- Events older than the plan's retention are removed nightly.
- A person deletion is a visible job; the console shows when it is due and
  when it finished. Export gives one JSON file per person.

## Tests

```sh
pnpm test          # scratch Postgres from initdb, never a hosted database
pnpm test:browser  # one Chromium run from the local browser cache
pnpm capacity      # memory at 50 events per second for 10 minutes
pnpm test:qy       # includes test/qy/mixpanel-compat.test.ts, the real mixpanel npm client against this service
```

## Docs

- `docs/MIXPANEL-PARITY-CHECKLIST.md` -- ingestion and analysis parity against another event-analytics product's wire protocol: what's compatible today, what's accepted but silently dropped, and what's still missing.

## Layout

- `src/capture/**` decoding, validation, caps, the insert buffer, the meter
- `src/persons/**`, `src/groups/**` identify, alias, merge, groups
- `src/tenancy/**` the site directory, console tokens, settings
- `src/gdpr/**`, `src/jobs/**` privacy jobs, retention, the scheduler
- `src/flags/**`, `src/remote-config/**` flag answers and app config
- `src/insights/**` and `src/{replay,heatmaps,surveys,experiments,exports}/**`
  are mounted by `src/routes.ts` and filled by the next packages
