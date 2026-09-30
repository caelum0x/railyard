# Product analytics: capacity note (2026-09-30)

- Status: **not deployed**.
- Every number below is an estimate until the package named in its row measures it and replaces the estimate. Estimates carry "est."; measured values carry the date and the command used.

## Box 2 budget (from `WAVE-P-PARITY-DISPATCH-2026-09-29.md` section 8)

- Box 2 has 11.6 GiB of RAM and is gated at **2 GB `MemAvailable` after each install**.
- The dispatch already estimates the Wave P sidecars at about 2.9 GiB in total. It says the gate is expected to hold for P1 and P2 but **not for all of P3**.
- So product analytics must be small, and it waits in line behind the gate like every P3 sidecar.

| Component | Where | Cap | Basis | Measured by |
|---|---|---|---|---|
| `product-analytics` service (Node 22, Hono, one process: capture, query, jobs) | box 2 | **192 MiB** (A1 measured 128.5 MiB max, see Measurements) | One Node process with a 2 s / 1 000-row insert buffer; no Kafka, Redis or ClickHouse | A1: RSS at 50 events/s for 10 min on the Mac; A3: with replay ingest and one export running |
| Postgres database `product_analytics` | box 2's existing Postgres (the Scribase cell) | no new process; shares `shared_buffers`; insight transactions use `work_mem = 32MB`, at most 5 at once (pool size), so at most about 160 MB of sort memory | Monthly partitions; hourly count rollups and a daily people rollup | A2: measured, see Measurements (all three budgets met) |
| Replay chunks | S3-compatible object storage (Scribase Storage S3 API or R2) | no RAM on box 2 | gzip rrweb chunks; 10% sample and 30-minute maximum (targets) | A3: bytes per recorded minute on the fixture |
| Console screens | Cloudflare Workers (existing console) | none on the boxes | | |

## Rejected for now

- **ClickHouse.** There is no measured room for it next to the P3 sidecars, which already exceed the gate on paper. It comes back only if a measurement on staging shows it fits under the gate together with the P3 sidecars that are running.
- **DuckDB in-process.** It is kept as the fallback for heavy queries (PLAN section 6), with `memory_limit` set below the service cap. It is not used by default.

## If the gate fails

- The order of waiting is the dispatch's order, with this service added **before** Shlink: Channel Engine, MediaMTX, VictoriaLogs, rspamd, product-analytics, then Shlink.
- The fallback while it waits: Insights (cookieless page analytics) keeps working, and the product-analytics console page says "arriving".
- The third box (owner item Y6) removes the wait.

## Storage (measured by A2 on 2026-09-30)

On the 1 000 000-event fixture (`test/a2/fixtures.ts`: 9 927 people over 90 days, page views, screens, clicks with elements chains, signups, purchases, 30% of people merged from an anonymous id):

| Part | Bytes per event | Note |
|---|---|---|
| Event rows (heap) | 248 | `pg_relation_size` over the monthly partitions |
| Indexes on events | 301 | A1's five indexes plus A2's covering index (73) and inserted-at index (9) |
| **Events total** | **549** | `pg_total_relation_size` over the partitions |
| Daily people rollup (A2) | 58 | 335 564 rows, 57.7 MB; one row per (event, UTC day, person) |
| Hourly count rollup (A2) | 2 | 1.6 MB |

- Monthly growth at the Railyard plan's event limit (100 000 stored events a month, `src/plans.ts`): about 61 MB a month (computed: 100 000 x 607 bytes), about 730 MB for the 12-month retention target.
- A1's `events_project_event_ts_idx` (24 bytes per event) is made redundant by A2's covering index; dropping it is a hand-off (`oss/A2.md`).
- The fixture was run on the Mac with `df -h /` checked first. The Mac was under the 10 GB free line when A2 started, so nothing was installed.

## Measurements

| Date | Package | What | Command | Result |
|---|---|---|---|---|
| 2026-09-30 | A1 | Service RSS (outside view, `ps -o rss`) at 50 events/s for 10 min: 5 gzip batches of 10 per second over 200 people, identify on first sight, one group per event; writer and scheduler running; scratch Postgres 14.18 on the Mac; Node v24.3.0 with `--max-old-space-size=128` | `cd apps/product-analytics && node test/a1/capacity.ts` | **max 128.5 MiB** (start 121.4 MiB, end 128.5 MiB) against the 192 MiB cap; 30 000 sent, 30 000 stored, 0 failed |
| 2026-09-30 | A2 | Insight latency on 1 000 000 generated events in one project, 20 runs each through the real runner (row level security, bound parameters, `work_mem` 32MB), after one rollup run. Scratch Postgres 14.18 (Homebrew, default `shared_buffers` 128MB) on the Mac (arm64, 8 cores), **shared with other work: load average 44 to 37 during the run** | `node test/a2/perf-cluster.ts start`, then `PA_CAPACITY_URL=<printed url> node test/a2/capacity.ts 1000000 20` | p95: **trends 30 d count 13 ms** (hourly rollup), **trends 30 d unique people 925 ms**, trends 30 d unique people by country 868 ms (budget 1.5 s: met); **3-step funnel, 14-day window 1 913 ms** (budget 3 s: met); **8-week retention 640 ms** (budget 3 s: met); lifecycle 30 d 1 180 ms; stickiness 30 d 720 ms; paths 30 d, depth 5: 5 026 ms (no budget in the plan; over the 3 s we aimed for, see below). Service RSS during the run 45 MiB |

Notes on the A2 measurement:
- Earlier runs at load averages of 60 to 180 on the same machine were 3 to 10 times slower; the numbers above are the ones taken at the lowest load of the day, and they are wall-clock times, not CPU times.
- What got the budgets met (each a rollup or an index first, as the plan asks): a covering index `(project_id, event, timestamp) include (person_id)` (the retention plan went from 2.8 s to 0.9 s in `EXPLAIN ANALYZE` at the same load), a daily people rollup (unique people went from 1.8 s to 0.3 s p50, measured at different loads, so the ratio is approximate), pruning funnel attempts that a later attempt always beats (a 3-step funnel joins about 17 000 entries instead of 340 000), grouping sets and hash deduplication in trends, and `work_mem` 32MB so sorts stay in memory.
- DuckDB stays the recorded fallback; it was not needed for the three budgets.
- Paths is the slowest insight: it sorts every page view of the range by person and time once. The next step if it matters is a per-session path rollup; it has no budget in the plan.

