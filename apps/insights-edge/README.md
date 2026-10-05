# Railyard Insights edge collector

A Cloudflare Worker (RYP-D3) that takes page views and custom events from the
`/s.js` page tag and hands them to Railyard Insights in batches.

- `POST /api/send` and `POST /api/batch` (up to 100 events), in the same shape as
  the widely used open-source page-analytics payload, so existing tags work.
- `GET /s.js`: the page tag. No cookies, no local storage, nothing sent when Do Not
  Track is on.
- Site-key check against the `SITES` KV entries the console writes.
- Bots, crawlers, link previews and prefetches are answered 202 and dropped.
- The visitor id is a salted hash that changes every UTC day. The IP is used only
  to compute it, in memory; it is never stored, logged or sent on.
- Plan caps per site and month. Over the cap the answer is still 202, with
  `x-railyard-capped: 1`; those events are counted for usage but not stored.
  A paused org gets `x-railyard-paused: 1`.
- One Durable Object per org buffers events and flushes every 2 seconds in one
  request, so each site sees at most one downstream request every 2 seconds.
- Once a minute each org's counts go to the usage meter as `insights_events`
  ($0 until paid overage is switched on).
- Page speed (QY1.2): the page tag sends each page view's LCP, INP and CLS (and
  FCP and TTFB) once, as a `performance` event, when the page is hidden.
  `data-vitals="off"` on the script tag turns it off. The tag stays under 3 KB gzip.
- With `PRODUCT_ANALYTICS_URL` set, every stored event also goes to product
  analytics in its capture protocol (page views as `$pageview`, page speed as
  `$web_vitals`, a cookieless daily id for anonymous visitors), so the web view
  there reads the same store as funnels and replay.

The downstream contract is `contract/INGEST-API.md` (and its JSON schema). Until
Insights ingest is deployed, `INGEST_URL` is empty and events are counted and
capped but not forwarded.

## Tests

```sh
pnpm install --filter @railyard/insights-edge
node --test test/*.test.mjs        # workerd (local runtime) tests plus a 60 s simulated-clock run
node test/load.mjs 60 1000         # load run in the local runtime; results in LOAD-RESULT.md
```

## Owner steps

1. `wrangler kv namespace create SITES` and put the id in `wrangler.jsonc`.
2. `wrangler secret put VISITOR_SALT_SECRET` (32+ random bytes), `INGEST_TOKEN`, `RAGO_API_KEY`.
3. Deploy at the Wave E gate, after Insights ingest is up.
