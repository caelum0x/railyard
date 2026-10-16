# Deploying railyard-console

The console is code-complete (typecheck clean, full test suite green).

**Correction, 2026-10-17:** a Worker deploy already happened on 2026-10-14
(`wrangler deployments list --name railyard-console` shows the first version at
2026-10-14T11:36Z, last at 11:43Z; 4 of the 9 `wrangler secret put` secrets are
set: `RAILYARD_CREDENTIALS_KEY`, `RAILYARD_SESSION_SECRET`, `SCRIBASE_ANON_KEY`,
`SCRIBASE_SERVICE_ROLE_KEY`). `https://railyard.ereboslab.com/` is live right
now (200, renders the sign-in page) — the "never deployed" framing below is
stale. **But sign-in does not work**: verified live with a same-origin
`POST /auth/email`, which redirects to `/?signin=failed`. Root cause, verified
two ways:
1. `SCRIBASE_URL` is set to `https://api.scribase.com` — the shared
   control-plane (management) API, not a project's own gateway host. Direct
   curl confirms `POST https://api.scribase.com/auth/v1/otp` 404s
   (`route_not_found`, "the requested management API route does not exist").
2. No Scribase project exists for the console at all yet. Checked directly,
   read-only, against `scribase.projects` on the box
   (`docker exec scribase-db-1 psql -U postgres -d postgres -c "select id from
   scribase.projects where organization_id='ereboslab';"`): **zero rows**. The
   `ereboslab` organization itself exists (created 2026-09-26) but owns no
   project at all — not for the console, not for any of the 9 mobile apps
   `scribase-move-apps.sh` targets either.

`lib/config.ts` now refuses to boot with `SCRIBASE_URL=https://api.scribase.com`
(see `test/config-billing.test.ts`, "SCRIBASE_URL must be a project's own
gateway host") so this exact misconfiguration fails loudly on the next deploy
instead of silently shipping a console that renders but can never sign anyone
in. **This has not been redeployed yet** — the live Worker still runs the old
config without the guard, still 404ing the same way, because fixing it for
real needs owner step 2 below (a real project) first; deploying the guard
alone first would only turn today's "page loads, sign-in silently fails" into
"page 500s," which is worse. Deploy the guard together with a real
`SCRIBASE_URL`, not before it.

**Owner-gated: the real fix (a Scribase project for this console) has not
been done.** `db/migrations/0001_railyard_console.sql`'s own header says this
project is an owner step — confirmed correct, not an oversight: provisioning
it for real needs a valid `SCRIBASE_ACCESS_TOKEN` for the `ereboslab`
organization (`npx scribase login` or the console's own Tokens page once one
exists), and no working one was found anywhere on this machine (the one
`SUPABASE_ACCOUNT_TOKEN` in `~/products/.env` is a different, unrelated
credential — tried against `api.scribase.com`, got 401). The rest of this file
is the exact sequence once the owner has that token and a provisioned
project's `api_url` / `anon_key` / `service_role_key`.

## Pre-flight checklist

- [ ] `RAILYARD_DODO_LIVE` is **unset** (or anything other than exactly `"1"`).
      `lib/config.ts`'s `readConfig` throws if `DODO_API_BASE` resolves to the
      live host (`https://live.dodopayments.com`) while this isn't `"1"` —
      covered by `test/config-billing.test.ts`. First deploy ships in **test
      mode only**; do not set this until a real card check + refund have been
      run in test mode and recorded (README "Owner steps" item 4).
- [ ] `DODO_API_BASE` is unset or `https://test.dodopayments.com` (the default).
- [ ] The Scribase project's `0001`–`0003` migrations are applied and `railyard`
      is in that project's exposed schemas (README "Owner steps" item 2).
- [ ] `npm run typecheck && npm test` pass locally (see below).
- [ ] `railyard.ereboslab.com`'s zone exists on our Cloudflare account — the
      `routes` entry in `wrangler.jsonc` is a `custom_domain` attach, not a new
      DNS record; it fails loudly at deploy time if the zone isn't there.

## Secrets (`wrangler secret put <NAME>`, once, interactively — never pass values as CLI args or commit them)

4 of the 9 are already set on the live Worker (`wrangler secret list --name
railyard-console`): `RAILYARD_CREDENTIALS_KEY`, `RAILYARD_SESSION_SECRET`,
`SCRIBASE_ANON_KEY`, `SCRIBASE_SERVICE_ROLE_KEY`. The last two are very likely
stale/placeholder — no Scribase project exists for them to belong to (see the
correction above) — so set real values for all four once the real project
exists; `wrangler secret put` overwrites in place. Run the rest from
`apps/console/`, in any order:

```sh
wrangler secret put DODO_API_KEY
wrangler secret put DODO_WEBHOOK_SECRET
wrangler secret put SCRIBASE_ANON_KEY
wrangler secret put SCRIBASE_SERVICE_ROLE_KEY
wrangler secret put RAILYARD_SESSION_SECRET            # 32+ random bytes
wrangler secret put RAILYARD_SCRIBASE_BRIDGE_SECRET    # whsec_... shared with Scribase control-api
wrangler secret put INSIGHTS_API_TOKEN
wrangler secret put RELAY_TENANT_KEY                   # from relay-tenancy.sh tenant-key
wrangler secret put RAILYARD_CREDENTIALS_KEY           # 32 random bytes, base64
```

These 9 are the ones named in `wrangler.jsonc`'s header comment and enforced by
`lib/config.ts`'s `SECRET_ENV` list (checked at build time by
`test/bundle-secrets.test.ts` so none can leak into the client bundle). The
console still starts without the Dodo three (`DODO_API_KEY`,
`DODO_WEBHOOK_SECRET`, plus the `DODO_PRODUCT_ID_*` vars below) — billing just
answers "card payments open soon" until they're set (README "Billing optional").

Optional, feature-gated secrets (`GITHUB_APP_*`, `CLOUDFLARE_OAUTH_*`,
`FLAGS_ADMIN_TOKEN`, `WATCH_AGENT_TOKEN`, `RALLY_TX_KEY`) are not required for
first deploy — the features they unlock stay off (503/hidden) without them.
See the README's Environment table for what each one does.

## Vars and bindings already in `wrangler.jsonc`

`RAILYARD_CONSOLE_URL`, `INSIGHTS_API_URL`, `RELAY_URL`,
`RAILYARD_INSIGHTS_URL`, `DODO_API_BASE` (test host), the `SITES` KV namespace,
and the `railyard.ereboslab.com` custom-domain route need no action.
**`SCRIBASE_URL` does — this is the one still wrong.** It is committed as
`https://api.scribase.com`, which is the control-plane API, not this
console's project gateway host; `lib/config.ts` now refuses to boot with that
exact value (see "Correction" above). Once the project exists, replace it in
`wrangler.jsonc`'s `vars` block with the real `api_url` the project's
`GET .../environments/production/api-keys` call returns (shape
`https://<host_label>.scribase.com`, per the box's
`SCRIBASE_GATEWAY_BASE_URL_TEMPLATE`) before the next deploy.
Set `DODO_BRAND_ID` and `DODO_PRODUCT_ID_CARD_CHECK` /
`DODO_PRODUCT_ID_MONTH` / `DODO_PRODUCT_ID_YEAR` as plain `vars` (not secrets)
once the owner creates the `railyard` Dodo brand and its three test-mode
products (README "Owner steps" item 1) — add them to `wrangler.jsonc`'s `vars`
block in the same PR that flips billing on.

## Build and deploy

```sh
cd apps/console
pnpm install --filter @railyard/console
npm run cf:build     # opennextjs-cloudflare build — reads open-next.config.ts
npm run cf:deploy     # opennextjs-cloudflare deploy — runs `wrangler deploy` under the hood
```

(`cf:build` / `cf:deploy` are the actual `package.json` scripts; there is no
separate `wrangler deploy` to run by hand — `cf:deploy` wraps it.)

## Verify before telling anyone it's live

```sh
curl -sI https://railyard.ereboslab.com/ | head -1        # expect 200/30x, not 522/1016
curl -s https://railyard.ereboslab.com/api/v1/health       # if this route exists; else load the sign-in page
```

Confirm sign-in (email OTP at minimum — Google/GitHub need the owner's OAuth
app redirect URL set first, README item 3), then run one $1 test-mode card
check and one refund and record the result (README "Owner steps" item 4)
**before** ever setting `RAILYARD_DODO_LIVE=1`.

## Rollback

A Worker version already exists (deployed 2026-10-14; see the correction
above), so prefer `wrangler rollback --name railyard-console` to the previous
deployment id (`wrangler deployments list --name railyard-console` to find
it) over either of the below — that's a clean revert instead of a
detach/delete.

- **Fastest, safest:** detach the custom domain without deleting the Worker —
  remove the `routes` entry from `wrangler.jsonc`, redeploy (`npm run
  cf:deploy`). `ereboslab.com`'s other routes (insights-edge, relay, etc.) are
  untouched; `railyard.ereboslab.com` stops resolving to this Worker.
- **Full removal:** `wrangler delete --name railyard-console` removes the
  Worker entirely. Re-deploying after this is identical to a first deploy.
