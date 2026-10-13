# Railyard console

The signed-in console for Railyard (RYP-D1, Wave E package Y-2): sign in with
Google or GitHub, a $1 card check that is credited back, a 14-day trial with the
plan's limits, then one plan billed monthly or yearly. Three screens (Apps,
Usage, Billing), one primary action on each, a pause switch and one-click cancel.

## How it works

- **Sign-in:** the console's own Scribase project handles Google and GitHub
  (OAuth with PKCE) and email: a one-time code mailed by Scribase Auth
  (`POST /otp`, then `POST /verify`, `lib/email-signin.ts`). Email sign-in needs
  no OAuth app, so the console can open before the Google and GitHub apps exist
  (`RAILYARD_OAUTH_PROVIDERS=` hides their buttons). The console reads the user from the auth server, creates the
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
- **Billing optional:** without all five Dodo values (key, webhook secret,
  three product ids) the console still runs. Sign-in, Scribase add-on orgs and
  every slot work; checkout, pause and cancel answer "card payments open soon"
  and the webhook answers 503.

## Environment

| Name | Where | What |
|---|---|---|
| `SCRIBASE_URL` | var | The console's Scribase project URL |
| `SCRIBASE_ANON_KEY` | secret | Project anon key (sign-in calls only) |
| `RAILYARD_OAUTH_PROVIDERS` | var | OAuth buttons to show, comma separated (default `google,github`; empty = email sign-in only) |
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
| `FLAGS_API_URL` | var | Railyard Flags (Wave P PY2.1): base URL of the flags service on box 2 (through the tunnel). With `FLAGS_ADMIN_TOKEN` set, new flag environments are made there; without them, flags live in the console's own tables (migration 0005), the fallback. |
| `FLAGS_ADMIN_TOKEN` | secret | The flags service's instance admin token (`Token ...` auth). Only this console holds it; apps read flags with their public flags key. |
| `RAILYARD_WATCH_LIVE` | var | Railyard Watch (Wave P PY2.2). `1` turns on uptime checks, the Watch screen, the public status pages and the alert email. Unset (the default): no check runs and nothing about Watch is shown (denylist rule `railyard-slot-watch`). Set it only after the customer checks stack on box 1 passes its drill. |
| `WATCH_AGENT_TOKEN` | secret | Bearer token of the box 1 checks agent (`stacks/gatus-customers/agent.sh`), at least 32 characters; the same value sits in `/etc/products/watch-agent.token` on box 1. Unset keeps the agent API off (503). |
| `RAILYARD_REPLAY_LIVE` | var | Insights depth (Wave P PY3.2) and web vitals (PY3.3). `1` serves `/api/v1/insights/*` (profiles, cohorts, retention by property, breakdowns, CSV and JSON export, delete by user id) to an org's `ry_` token, and the vitals views. Unset (the default): those answer 503 "arriving" (denylist rule `railyard-slot-replay`). Identify events are stored either way. |
| `RAILYARD_SHIP_ON_PUSH_LIVE` | var | Ship on push (Wave P PY3.1, ADR RY-0004). `1` turns on `railyard link`, the GitHub and Cloudflare browser steps and the `railyard ci` exchange. Unset (the default): every `/api/v1/github/*` route answers 503 "not switched on" (denylist rule `railyard-slot-ship-on-push`). |
| `GITHUB_APP_ID`, `GITHUB_APP_SLUG` | var | The Railyard GitHub App (owner item Y4, created from `/api/v1/github/manifest`). |
| `GITHUB_APP_PRIVATE_KEY`, `GITHUB_APP_WEBHOOK_SECRET` | secret | The App's key (PKCS#1 as GitHub issues it, or PKCS#8) and its webhook secret. Stored by the command the manifest page shows; never displayed. |
| `GITHUB_APP_CLIENT_ID`, `GITHUB_APP_CLIENT_SECRET` | var, secret | The App's own sign-in client. `railyard link` has the user sign in with the App and checks they can push to the repository before linking it. Stored by the same manifest command; the App's callback URL is `<console>/api/v1/github/user/callback`. Ship on push stays off without them. |
| `CLOUDFLARE_OAUTH_CLIENT_ID` | var | Our Cloudflare OAuth client (public, domain verified), redirect `<console>/api/v1/github/cloudflare/callback`. |
| `CLOUDFLARE_OAUTH_CLIENT_SECRET` | secret | Its client secret. |
| `CLOUDFLARE_OAUTH_SCOPES` | var | Optional: the scope ids configured on that client, space or comma separated. Default: account read, user read, Workers and Workers scripts and routes write, `offline_access`. |
| `RALLY_TX_URL` | var | Rally Send transactional endpoint (https) for Watch alert email, product `railyard`. |
| `RALLY_TX_KEY` | secret | Rally Send key minted for product `railyard`. Without it, alerts are logged, not sent. |
| `INSIGHTS_API_TOKEN` | secret | Bearer token of the Insights tenant API |
| `SITES` | KV binding | The edge collector's site entries; written when an app is added and on every plan, add-on or pause change |
| `RELAY_URL` | var | Relay's OpenAI-compatible base for the AI gateway (Wave P PY1.4) |
| `RELAY_TENANT_KEY` | secret | The Relay tenant key from `relay-tenancy.sh tenant-key` (BYOK only; see `apps/relay/config/tenants/README.md`) |
| `RAILYARD_CREDENTIALS_KEY` | secret | 32 random bytes, base64: seals customers' provider keys |
| `RAILYARD_SCRIBASE_BRIDGE_SECRET` | secret | Standard Webhooks secret (`whsec_...`) shared with Scribase control-api for the bridge (Wave P PY1.0). Unset keeps the bridge off (503). |
| `RAILYARD_BRIDGE_ACCEPT_LEGACY` | var | P-Y1 transition only. `1` also accepts bridge calls from a Scribase build older than contract version 2 (message id without the route, `orgs` without `addon.as_of`). Unset (the default) refuses them. Set it only for the minutes between the console deploy and the Scribase control-api deploy, then remove it. |

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

## Deploy

Never deployed yet. See [`DEPLOY.md`](./DEPLOY.md) for the exact secret list,
build/deploy commands, pre-flight checklist, and rollback.

## Owner steps

1. Create the `railyard` brand and the three test-mode products; set the env above.
2. Apply `db/migrations/0001_railyard_console.sql`, then
   `db/migrations/0002_scribase_bridge.sql` and `db/migrations/0003_ai_gateway.sql`, to the console's Scribase project
   and add `railyard` to its exposed schemas.
3. Email sign-in works as soon as the project mails (its SMTP or Rally Send).
   Later, enable Google and GitHub sign-in on that project with
   `https://railyard.ereboslab.com/auth/callback` as a redirect URL and drop
   `RAILYARD_OAUTH_PROVIDERS`.
4. Point the webhook at `/api/billing/webhook`, then run one card check and one
   refund in test mode and record the result.
