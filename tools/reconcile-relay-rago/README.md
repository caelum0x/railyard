# Relay and Rago shadow-metering reconciliation (RY-RG1)

Rago runs as a $0 shadow next to each product's own billing. Before any product line
is invoiced from Rago (RY-RG4, after 60 days of drift under 0.5%), every charged Relay
inference must be shown to reach Rago exactly once, at the same amount. This job
checks that every night.

## What it compares

For one UTC day (default: yesterday), event by event:

| Source | Table | Key |
|---|---|---|
| Relay (`relay` database) | `relay_cost_events` (the durable outbox; `event->>'charged_micros'`, `settled_at_ms`, `published_at`, `dead_lettered_at`) | `event_id` |
| Rago (`rago` database) | `events` where `code = 'relay_llm_usage'` and not deleted (`properties->>'charged_micros'`, `organization_id`) | `transaction_id` (the feed sets it to the Relay `event_id`) |

| Finding | Meaning | Counts as drift |
|---|---|---|
| missing | Relay published it, Rago has no row (a dropped event) | its full charge |
| dead-letter | Relay gave up delivering it | its full charge, and **pages** |
| mismatch | both have it, amounts differ | the difference |
| duplicate | billed in more than one Rago org | every extra copy, and **pages** |
| orphan | Rago row for that day with no Relay row | its charge |
| pending overdue | not delivered after `RECON_PENDING_GRACE_S` | only once `RECON_EXPECT_FEED=1` |

`ry_rago_drift_ratio` = drift micros / the day's Relay charged micros. Over
`RECON_DRIFT_PAGE` (0.005, i.e. 0.5%) it pages the `railyard` ntfy topic at high
priority. Any finding below that is sent at default priority, so one dropped event
is always reported. Every figure is pushed to VictoriaMetrics (`ry_rago_*`, per key
and per Rago org) and written to `/var/lib/railyard/reconcile-relay-rago/<day>.json`,
with up to 50 example event ids per finding.

It is read-only: both databases are read inside `BEGIN TRANSACTION READ ONLY`, through
`psql` in the shared Postgres container. No product's charge of record changes.

## Install (box 2, owner or deploy agent)

```bash
sudo /srv/src/railyard/tools/reconcile-relay-rago/install.sh        # timer at 04:20 UTC
sudo /srv/src/railyard/tools/reconcile-relay-rago/install.sh run    # one run now
```

Settings are in `/etc/products/reconcile-relay-rago.env` (template
`reconcile.env.sample`; no secrets). The ntfy topic comes from
`/etc/products/monitoring.env` (`NTFY_URL_RAILYARD`, else `NTFY_URL`).

Until the cost feed is switched on, Relay keeps its rows `pending`. The report then
says `feed_off: true`, `ry_rago_feed_off` is 1, and nothing pages.

## Emitter flip checklist (each lane flips its own env)

Nothing here changes a product's price or its billing processor: every Rago plan these
events land on is a $0 mirror. Do the steps in order. Each one is one env change and a
restart of that service.

| # | Lane and file | Lines | Proof it worked |
|---|---|---|---|
| 1 | Railyard: Rago tenants | `apps/rago/seeds/import.sh` for `autogtm.json` (Rally), `menivor-reels-shadow.json` (Menivor) and `scribase.json`; create one Rago API key per org with the tenant bootstrap CLI | `GET /api/v1/billable_metrics/relay_llm_usage` answers 200 in each org |
| 2 | Railyard: `/etc/products/relay.env` | `RELAY_RAGO_ROUTES_JSON={"<rally key id>":{"api_key_env":"RAGO_KEY_AUTOGTM","subscription_id":"autogtm-relay"},"<menivor key id>":{...},"<scribase key id>":{...}}` and one `RAGO_KEY_<ORG>=` line per route | checked in step 3 (Relay reads the routes only once the feed URL is set) |
| 3 | Railyard: `/etc/products/relay.env` | `RELAY_COST_FEED_URL=http://rago:3000/api/v1/events`, `RELAY_COST_FEED_FORMAT=rago` | Relay logs `Relay cost-feed delivery enabled` with `format=rago` (a malformed routes JSON stops startup); next day `ry_rago_feed_off` is 0; then set `RECON_EXPECT_FEED=1` in `reconcile-relay-rago.env` |
| 4 | Rally (`vcglass` env on box 2) | `RAGO_URL=https://rago.<railyard domain>`, `RAGO_API_KEY=<autogtm org key>`, `RAGO_SHADOW=1`; LLM calls through Relay: `RELAY_URL=` (see `docs/CONSUMERS.md`) | Rally `relay_llm_usage` rows appear under the autogtm org |
| 5 | Menivor (`videogen` API env) | `RAILYARD_RAGO_URL`, `RAILYARD_RAGO_API_KEY`, `RAILYARD_RAGO_SUBSCRIPTION_ID`; LLM through Relay: `RAILYARD_RELAY_URL`, `RAILYARD_RELAY_API_KEY` | Menivor rows appear under its org |
| 6 | Scribase (control-api env) | `SCRIBASE_RAGO_URL`, `SCRIBASE_RAGO_API_KEY`, `SCRIBASE_RAGO_SUBSCRIPTION_TEMPLATE` | usage-mirror events appear in the scribase org (not part of this LLM check) |

Rollback for any step is unsetting the same lines. Relay then keeps rows `pending`
again, and the report shows `feed_off`.

## Tests

```bash
python3 -m unittest discover -s tools/reconcile-relay-rago/test -v
```

Offline, with a fake `psql`, a local ntfy and a local VictoriaMetrics import endpoint.
They cover a clean day, a deliberately dropped event (reported), drift over 0.5%
(pages), a dead-letter (pages), mismatch, duplicate and orphan, the day window, the
feed-off state, fresh pending rows, read-only SQL, a database error and bad input.

The staging run (a real dropped event on staging data) needs Relay and Rago deployed
with the feed on. It is an owner or deploy-agent step: step 3 above, then delete one
Rago test event by hand in staging and check that the next run reports it.
