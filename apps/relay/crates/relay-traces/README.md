# relay-traces (RY-RL3, OSS9)

Per-API-key trace view for Relay, plus the trace retention job.

## Endpoints (gateway)

| Route | Who | Returns |
|---|---|---|
| `GET /relay/traces` | any gateway key | The caller's own traces, newest first: model, provider, tokens, latency, cost (micro-USD, or omitted when unpriced), finish reason, number of model calls. |
| `GET /relay/traces?scope=all[&key=<public_id>]` | keys in `RELAY_OPERATOR_KEY_IDS` | Every key, or one key. `key=` without `scope=all` is a 400. |
| `GET /relay/traces/{inference_id}` | owner key (or operator with `scope=all`) | One trace with every model call and the stored input/output. A trace owned by another key is a 404, the same as a missing one. |

Query parameters: `limit` (1-200, default 50), `window_hours` (1-2160, default 168), `before` (the `next_before` cursor), `function`.

Reads use the existing GIN index on `tags` (`tensorzero::api_key_public_id`) and are bounded by a time window, so Postgres prunes partitions. This crate adds **no tables and no migrations**; Postgres growth from RY-RL3 is zero, and the retention job reduces it.

The Relay UI page is `/observability/keys` ("Key Traces" in the sidebar). The first page refreshes every 5 s, so a new call shows up within 10 s of being written.

## Retention (owner decision OD-RY2)

`relay-traces-retention` deletes, in batches and against one cutoff taken at the start of the run:

- bodies (`chat_inference_data`, `json_inference_data`, `model_inference_data`) older than `RELAY_TRACE_BODY_RETENTION_DAYS` (default **14**);
- metadata (`chat_inferences`, `json_inferences`, `model_inferences`) older than `RELAY_TRACE_METADATA_RETENTION_DAYS` (default **90**).

Bodies go first. Missing tables are skipped. Body retention can never exceed metadata retention. TensorZero's own partition retention (`tensorzero.retention_config`) is left untouched.

Rehearse before enabling:

```bash
# inside the relay container (the image ships the binary)
relay-traces-retention --dry-run          # counts only, deletes nothing
relay-traces-retention --dry-run --schema <scratch_copy_schema>
```

`deploy/relay-traces-retention.{service,timer}` run it daily at 04:20 UTC via `docker compose -p railyard exec`. They are not installed by this repo.

## Tests

- Unit: `cargo test -p relay-traces` (params, cursor, scope rules, policy parsing).
- Live, on a scratch copy of the inference tables: `RELAY_TEST_DATABASE_URL=postgres://... cargo test -p relay-traces --test pg_live`. It proves key isolation (key B gets nothing for key A), operator scope, keyset paging, and that retention removes only rows older than each window.
