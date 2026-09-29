# Insights edge load test

Run by `node test/load.mjs 60 1000`.

```json
{
  "date": "2026-09-29T13:29:49.249Z",
  "runtime": "Miniflare 5 (workerd 1.20260923.1), local Mac, other lanes' builds running",
  "target": "1000 events/s for 60 s over 5 sites in 3 orgs",
  "sent": 60000,
  "elapsed_s": 475.4,
  "achieved_rate": 126,
  "late_seconds": 60,
  "statuses": {
    "202": 60000
  },
  "all_202": true,
  "downstream_requests": 590,
  "events_forwarded": 60000,
  "max_downstream_requests_per_site_per_second": 1,
  "raw_ip_downstream": false
}
```

**Read this before quoting the numbers.** All 60,000 events were answered 202, all
were forwarded exactly once, no site saw more than one downstream request in any
1-second window, and no IP reached a downstream body. The 1,000 events a second
rate was **not reached on this machine**: the Mac's load average was about 72
(other lanes were building), and a do-nothing request to the same local runtime
peaked at 195 requests a second, so the driver ran at 126 a second and took 475 s.
The rate target needs a re-run on an idle machine or with `wrangler dev` on a
quiet box (owner or verifier step). The 60-second, 1,000-a-second path is also
covered by `test/collector-sim.test.mjs` on a simulated clock.
