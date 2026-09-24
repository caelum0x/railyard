# Rago consumer seeds

Catalog fixtures — **billable metrics + a plan** — for each product that meters
usage through the shared rago billing plane. One seed file == one consumer ==
one rago **organization** (tenant).

| Seed | Consumer | Metric code(s) | Plan | Charge model |
|------|----------|----------------|------|--------------|
| `autogtm.json` | Rally AutoGTM (vcglass cold email) | `gtm_email_sent`, `gtm_draft`, `gtm_topup` | `gtm_usage` ($0 metering mirror, vcglass default) + `autogtm_growth` ($49/mo) | `standard` at $0 (mirror); `package` (emails) + `standard` (drafts) on growth |
| `menivor.json` | Menivor (video/reel + LLM) | `menivor_media_spend` | `menivor_studio` ($29/mo) | `dynamic` (cost pass-through) |
| `menivor-reels.json` | Menivor (reel generation + Relay LLM calls) | `reel_generated`, `reel_render_seconds`, `reel_ai_credits`, `menivor_llm_call` | `menivor_reels` ($0 base) | `standard` (per-unit; matches `RAGO_REEL_PRICING` defaults; LLM calls at $0) |
| `opentypesafe.json` | opentypesafe (System One / Jev) | `systemone_call` | `opentypesafe_pro` ($0 base) | `graduated` (per-call tiers) |

Each file is an envelope:

```jsonc
{
  "consumer": "autogtm",
  "billable_metrics": [ { "code": "...", "aggregation_type": "count_agg", ... } ],
  "plans":            [ { "code": "...", "charges": [ { "billable_metric_code": "...", "charge_model": "...", "properties": {...} } ] } ]
}
```

The field names match rago's REST request bodies exactly (Lago root-wrapped
shape): a metric maps to `POST /billable_metrics { "billable_metric": {...} }`
and a plan (with nested `charges`) maps to `POST /plans { "plan": {...} }`.
See `crates/rago-api/src/handlers.rs` (`create_billable_metric`, `create_plan`)
and the enums in `crates/rago-core/src/enums.rs` (`AggregationType`,
`ChargeModel`, `PlanInterval`).

## 0. One-time: create the consumer org + API key

Rago has no public signup; the Bearer token in `api_keys.value` selects the org.
Create one org + key **per consumer** directly in Postgres (run against the
`rago` database — see `../../DEPLOY-SHARED-PLANE.md`):

```sql
-- AutoGTM tenant (repeat per consumer with a fresh org id + token).
WITH org AS (
  INSERT INTO organizations (id, name, slug, created_at, updated_at)
  VALUES (gen_random_uuid(), 'AutoGTM', 'autogtm', now(), now())
  RETURNING id
)
INSERT INTO api_keys (id, organization_id, value, name, created_at, updated_at)
SELECT gen_random_uuid(), org.id, 'rago_live_autogtm_REPLACE_ME', 'autogtm-seed', now(), now()
FROM org;
```

Use a long random `value` (`openssl rand -hex 24`) in real deploys; keep it in
the consumer's secret store.

## 1. Import — script

```bash
RAGO_API_KEY='rago_live_autogtm_REPLACE_ME' ./import.sh autogtm.json
RAGO_API_KEY='rago_live_menivor_...'         ./import.sh menivor.json
RAGO_API_KEY='rago_live_opentypesafe_...'    ./import.sh opentypesafe.json

# Remote plane:
RAGO_API_URL=https://rago.example.com/api/v1 RAGO_API_KEY=... ./import.sh menivor.json
```

The script is re-run safe: a `422` (already exists / validation) is reported as
`skip`, not a failure. Requires `curl` + `jq`.

## 2. Import — raw curl (no script)

```bash
API=http://localhost:3000/api/v1
KEY='rago_live_autogtm_REPLACE_ME'

# a metric
curl -fsS -X POST "$API/billable_metrics" \
  -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"billable_metric":{"name":"GTM Emails Sent","code":"gtm_email_sent","aggregation_type":"count_agg"}}'

# a plan with nested charges
curl -fsS -X POST "$API/plans" \
  -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"plan":{"name":"AutoGTM Growth","code":"autogtm_growth","interval":"monthly","amount_cents":4900,"amount_currency":"USD","pay_in_advance":true,"charges":[{"billable_metric_code":"gtm_email_sent","charge_model":"package","properties":{"amount":"8.0","package_size":1000,"free_units":2000}},{"billable_metric_code":"gtm_draft","charge_model":"standard","properties":{"amount":"0.02"}}]}}'
```

## 3. Verify

```bash
curl -fsS -H "Authorization: Bearer $KEY" "$API/plans"            | jq '.plans[].code'
curl -fsS -H "Authorization: Bearer $KEY" "$API/billable_metrics" | jq '.billable_metrics[].code'
```

## 4. Emit usage (downstream, per consumer)

After a customer + subscription exist for the plan, meter usage with `POST /events`:

```bash
curl -fsS -X POST "$API/events" -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"event":{"transaction_id":"'"$(uuidgen)"'","external_subscription_id":"sub_123","code":"gtm_email_sent"}}'
```

For Menivor's `dynamic` charge, carry the real provider cost on each event:

```bash
-d '{"event":{"transaction_id":"...","external_subscription_id":"sub_123","code":"menivor_media_spend","properties":{"units":12},"precise_total_amount_cents":"340"}}'
```
