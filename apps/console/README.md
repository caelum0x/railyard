# Railyard console

The signed-in console for Railyard (RYP-D1, Wave E package Y-2): sign in with
Google or GitHub, a $1 card check that is credited back, a 14-day trial with the
plan's limits, then one plan billed monthly or yearly. Three screens (Apps,
Usage, Billing), one primary action on each, a pause switch and one-click cancel.

## How it works

- **Sign-in:** the console's own Scribase project handles Google and GitHub
  (OAuth with PKCE). The console reads the user from the auth server, creates the
  org and its entitlements row, and keeps a signed, HttpOnly session cookie.
- **Data:** schema `railyard` in the same Scribase project
  (`db/migrations/0001_railyard_console.sql`), reached with the service role
  from the server only. RLS is on with no policies.
- **Money ladder:** `lib/ladder.ts` (pure). Checkout sessions carry signed
  metadata, so a payment link with made-up metadata cannot name an org.
  `lib/webhook.ts` verifies the Standard Webhooks signature and timestamp,
  ignores replays (unique message id), applies the ladder, and credits or debits
  the $1 on the customer's wallet with idempotency keys. A refund of the card
  check reverses the credit and ends the trial.
- **CLI sign-in:** a device flow at `/api/v1/device/code` and
  `/api/v1/device/token`; the user approves the code at `/device`. Tokens are
  org-scoped and stored as SHA-256 hashes.
- **Test mode only:** the payment client refuses the live host unless
  `RAILYARD_DODO_LIVE=1` is set by the owner.

## Environment

| Name | Where | What |
|---|---|---|
| `SCRIBASE_URL` | var | The console's Scribase project URL |
| `SCRIBASE_ANON_KEY` | secret | Project anon key (sign-in calls only) |
| `SCRIBASE_SERVICE_ROLE_KEY` | secret | Project service role (schema `railyard`) |
| `RAILYARD_SESSION_SECRET` | secret | 32+ random bytes; signs sessions and checkout metadata |
| `DODO_API_KEY` | secret | Test-mode key of the `railyard` brand |
| `DODO_WEBHOOK_SECRET` | secret | Webhook signing secret (`whsec_...`) |
| `DODO_BRAND_ID` | var | Brand id; events for other brands are ignored |
| `DODO_PRODUCT_ID_CARD_CHECK`, `DODO_PRODUCT_ID_MONTH`, `DODO_PRODUCT_ID_YEAR` | var | The three products |
| `DODO_API_BASE` | var | Defaults to the test host |
| `RAILYARD_DODO_LIVE` | var | Owner only: `1` allows the live host |
| `RAILYARD_CONSOLE_URL`, `RAILYARD_INSIGHTS_URL` | var | Public URLs |
| `INSIGHTS_API_URL` | var | Insights tenant API base (Wave P PY1.3); empty keeps the Insights page on "No data yet" |
| `INSIGHTS_API_TOKEN` | secret | Bearer token of the Insights tenant API |
| `SITES` | KV binding | The edge collector's site entries; written when an app is added and on every plan, add-on or pause change |
| `RELAY_URL` | var | Relay's OpenAI-compatible base for the AI gateway (Wave P PY1.4) |
| `RELAY_TENANT_KEY` | secret | The Relay tenant key from `relay-tenancy.sh tenant-key` (BYOK only; see `apps/relay/config/tenants/README.md`) |
| `RAILYARD_CREDENTIALS_KEY` | secret | 32 random bytes, base64: seals customers' provider keys |
| `RAILYARD_SCRIBASE_BRIDGE_SECRET` | secret | Standard Webhooks secret (`whsec_...`) shared with Scribase control-api for the bridge (Wave P PY1.0). Unset keeps the bridge off (503). |

## Scribase bridge (Wave P PY1.0)

Scribase sells Railyard inside its portfolio add-on. Its control-api calls
`/api/v1/bridge/scribase/{orgs,claims,proof}`, server to server and signed
(`app/api/v1/bridge/scribase/service.ts`). The first call creates the Railyard
org bound to the Scribase org id (no second sign-up, table
`railyard.scribase_bindings`, migration `0002`), and the add-on state is
mirrored into the entitlement as plan `addon`. A lapsed add-on pauses the org
and keeps its data. Keys reach only the Scribase CLI: a claim is a one-time,
pre-approved device code that the CLI exchanges at `/api/v1/device/token` and
writes to the credentials file. The contract is
`tests/contract/fixtures/scribase-bridge/contract.json` at the repo root.

## Tests

```sh
pnpm install --filter @railyard/console
node --test test/*.test.ts        # journey, webhook, ladder, tokens, repo, component, bundle, migration
```

The journey test runs sign-in, the $1 checkout, the webhook and "Add app" against
in-memory fakes of the payment and auth servers. The bundle test runs
`next build` with sentinel secrets and greps the client bundle. The migration
test starts a throwaway local Postgres.

## Owner steps

1. Create the `railyard` brand and the three test-mode products; set the env above.
2. Apply `db/migrations/0001_railyard_console.sql`, then
   `db/migrations/0002_scribase_bridge.sql` and `db/migrations/0003_ai_gateway.sql`, to the console's Scribase project
   and add `railyard` to its exposed schemas.
3. Enable Google and GitHub sign-in on that project with
   `https://railyard.ereboslab.com/auth/callback` as a redirect URL.
4. Point the webhook at `/api/billing/webhook`, then run one card check and one
   refund in test mode and record the result.
