# Insights ingest API contract (edge collector to Insights)

Version 1, 2026-09-29; version 1.1 (additive, PY3.2), 2026-09-30. Written by RYP-D3 (the edge collector, `apps/insights-edge`);
implemented later by RYP-D2 (Insights, box 2). The collector is the only caller.
Machine-readable schema: `ingest-batch.schema.json`.

## Request

`POST {INGEST_URL}/v1/ingest/batch`

- `authorization: Bearer {INGEST_TOKEN}` (a shared secret; rotate by setting a
  second accepted token on Insights first).
- `content-type: application/json`
- At most one request every 2 seconds per org, so at most one every 2 seconds per
  site. Bodies stay under 10,000 events per site.

```json
{
  "org_id": "0b8f6a3e-...",
  "sent_at": "2026-09-29T12:00:02.000Z",
  "batches": [
    {
      "site_key": "ry_site_...",
      "app_id": "5d1c...",
      "events": [
        {
          "type": "event",
          "name": null,
          "url": "/pricing?ref=x",
          "hostname": "shop.example",
          "referrer": "https://news.example/",
          "title": "Pricing",
          "language": "en-US",
          "screen": "1440x900",
          "data": null,
          "visitor": "3f1e...32 hex",
          "country": "DE",
          "ts": "2026-09-29T12:00:01.512Z"
        }
      ]
    }
  ]
}
```

- `name` null is a page view; a string is a custom event with optional `data`
  (up to 20 string, number or boolean values).
- Version 1.1: `type` may also be `"identify"`, and any event may carry `user`, the
  app's own id for a signed-in person (1 to 50 of `A-Z a-z 0-9 @ . _ : + = -`). An
  identify must carry `user`; its `data` are the person's traits (merged into their
  profile). Insights links the visitor's session to the user and stores no event row
  for an identify. A version 1 collector never sends either field.
- `visitor` is `HMAC(daily_salt, site_key, ip, user_agent)` cut to 128 bits. The
  salt is `HMAC(VISITOR_SALT_SECRET, "railyard-insights-salt:" + UTC date)`, so a
  visitor cannot be linked across days or sites. **No IP address is ever sent**,
  and Insights must not add one.
- Every event in a batch is under the org's plan cap — one pool shared across
  every site/app of the org, not a separate cap per site. Over-cap events
  never reach Insights; they are counted in Rago only.

## Response

- `2xx`: all batches accepted. The collector drops them from its buffer.
- Anything else, or no answer: the collector keeps the events and retries at its
  next 2-second flush. Insights must de-duplicate a retried request by
  `(org_id, sent_at, site_key)`.

## Site entries (console to collector)

The console writes one KV entry per app (`SITES` namespace), key `site:<site_key>`:

```json
{ "org_id": "...", "app_id": "...", "events_limit": 100000, "paused": false }
```

`events_limit` comes from the org's entitlements (the same number on every
site/app of the org — the collector pools usage across all of an org's sites
against this one number, it is not a separate allowance per site); `paused`
from the pause switch. The collector caches an entry for 60 seconds. (Writing
these entries on app creation and on entitlement changes is RYP-E1.)

## Usage to Rago (shadow meter)

Once a minute per org: `POST {RAGO_URL}/api/v1/events` with
`{ "event": { "transaction_id": "insights:<org>:<minute>", "external_subscription_id": "railyard-org-<org>", "code": "insights_events", "timestamp": <unix>, "properties": { "events", "stored", "capped", "dropped" } } }`.
The meter is priced at $0 until RYP-E1.
