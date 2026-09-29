# Product analytics: capacity note (2026-09-30)

- Status: **not deployed**.
- Every number below is an estimate until the package named in its row measures it and replaces the estimate. Estimates carry "est."; measured values carry the date and the command used.

## Box 2 budget (from `WAVE-P-PARITY-DISPATCH-2026-09-29.md` section 8)

- Box 2 has 11.6 GiB of RAM and is gated at **2 GB `MemAvailable` after each install**.
- The dispatch already estimates the Wave P sidecars at about 2.9 GiB in total. It says the gate is expected to hold for P1 and P2 but **not for all of P3**.
- So product analytics must be small, and it waits in line behind the gate like every P3 sidecar.

| Component | Where | Cap | Basis | Measured by |
|---|---|---|---|---|
| `product-analytics` service (Node 22, Hono, one process: capture, query, jobs) | box 2 | **192 MiB** (est.) | One Node process with a 2 s / 1 000-row insert buffer; no Kafka, Redis or ClickHouse | A1: RSS at 50 events/s for 10 min on the Mac; A3: with replay ingest and one export running |
| Postgres database `product_analytics` | box 2's existing Postgres (the Scribase cell) | no new process; shares `shared_buffers` | Monthly partitions; rollups for counts | A2: query p95 on 1 000 000 events |
| Replay chunks | S3-compatible object storage (Scribase Storage S3 API or R2) | no RAM on box 2 | gzip rrweb chunks; 10% sample and 30-minute maximum (targets) | A3: bytes per recorded minute on the fixture |
| Console screens | Cloudflare Workers (existing console) | none on the boxes | | |

## Rejected for now

- **ClickHouse.** There is no measured room for it next to the P3 sidecars, which already exceed the gate on paper. It comes back only if a measurement on staging shows it fits under the gate together with the P3 sidecars that are running.
- **DuckDB in-process.** It is kept as the fallback for heavy queries (PLAN section 6), with `memory_limit` set below the service cap. It is not used by default.

## If the gate fails

- The order of waiting is the dispatch's order, with this service added **before** Shlink: Channel Engine, MediaMTX, VictoriaLogs, rspamd, product-analytics, then Shlink.
- The fallback while it waits: Insights (cookieless page analytics) keeps working, and the product-analytics console page says "arriving".
- The third box (owner item Y6) removes the wait.

## Storage estimate (est., to be replaced by A2's measurement)

- Growth is proportional to stored events: row size plus indexes with JSONB properties.
- A2 records the measured bytes per event on the 1 000 000-event fixture (`pg_total_relation_size` over the partitions) and the resulting monthly growth at the plan's event cap. It writes both here.
- The Mac disk rule applies to the fixture: run `df -h /` first and stay above 10 GB free.

## Measurements

| Date | Package | What | Command | Result |
|---|---|---|---|---|
| (none yet) | | | | |
