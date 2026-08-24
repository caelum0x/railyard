# Vend

**White-label metered inference gateway + reserve-backed wallet (Stripe-meets-OpenRouter).**

Vend is a REAL service: it calls the live **mint-server** (accounts / holds /
capture / release / reserve) and **Relay** (inference) over HTTP through
injectable transport TRAITS (faked only in tests). Multi-tenant, with
provisioning keys, per-key spend caps, and markup. The money ledger lives on
Mint; Vend's own persistence is Neon Postgres (sqlx runtime API, compile-verified
with no `DATABASE_URL`, live at deploy).

## Standalone workspace

`apps/vend` is its OWN cargo workspace, **excluded** from the root `Cargo.toml`
(like `apps/mint` / `apps/relay`) so it never enters the desktop GPUI/Metal build
graph. Build and test from here only:

```bash
cd apps/vend && cargo check     # never from the repo root
cd apps/vend && cargo test
```

## Crate graph

| crate | role |
|-------|------|
| `vend-core` | domain kernel, no I/O: integer-µ$ money math, `MarkupPolicy`/`SpendCap`/`FundingMode`, ULID id newtypes, `VendError` + envelopes, and the injectable `MintTransport`/`RelayTransport` traits + repo ports |
| `vend-tenancy` | Tenant/App/EndUser hierarchy + resolution over the `TenantRepo` port |
| `vend-keys` | provisioning-key lifecycle: mint `vk_live_`/`vk_test_` once, argon2id hash + prefix shard, verify, revoke, spend-cap gate |
| `vend-store` | Neon Postgres impls of every repo port via the sqlx runtime API + migrations |
| `vend-mint-client` | real reqwest client implementing `MintTransport` against mint-server |
| `vend-relay-client` | real reqwest client implementing `RelayTransport` against Relay |
| `vend-settlement` | reserve→capture/release plus durable, idempotent provider/tenant/platform split payout execution |
| `vend-server` | axum data plane binary wiring the real clients + store from env |
| `vend-testkit` | in-memory `FakeMint`/`FakeRelay`/`InMemoryStore` + fixtures for tests |

## Money math (integer µ$)

```text
markup_micros = max(ceil(provider_cost * markup_bps / 10_000), min_fee_micros)
charge        = provider_cost + markup
platform_fee  = ceil(markup * platform_fee_bps / 10_000)   // fee on the MARGIN
tenant_net    = markup - platform_fee
```

## Environment (deploy)

Copy `.env.example` and replace every `replace_...` value. `DATABASE_URL`,
`MINT_BASE`, `MINT_API_KEY`, `RELAY_BASE`, `RELAY_KEY`, and
`VEND_ADMIN_TOKEN_SHA256`, `VEND_RATE_LIMIT_PEPPER_HEX`,
`VEND_PUBLIC_BASE_URL`, `VEND_CLEARING_ACCOUNT`, `VEND_PROVIDER_PAYOUT_ACCOUNT`,
`VEND_PLATFORM_PAYOUT_ACCOUNT`, and `VEND_MODEL_RATES_JSON` are required.
`RELAY_BASE_PATH` defaults to `/openai/v1`;
`VEND_BIND_ADDR` defaults to `0.0.0.0:8090`. Rate-card prices are operator-owned
microdollars per million tokens; the checked-in example values are illustrative,
not a claim about current provider pricing.

Generate a high-entropy administrator token and configure only its digest:

```bash
ADMIN_TOKEN="$(openssl rand -hex 32)"
ADMIN_DIGEST="$(printf %s "$ADMIN_TOKEN" | shasum -a 256 | awk '{print $1}')"
export ADMIN_TOKEN VEND_ADMIN_TOKEN_SHA256="$ADMIN_DIGEST"
export VEND_RATE_LIMIT_PEPPER_HEX="$(openssl rand -hex 32)"
```

Save `ADMIN_TOKEN` in a secret manager. Vend never needs the plaintext in its
server environment and refuses to start if the digest is missing, malformed, or
all zeroes.

Administrator bearer rotation has an explicit overlap window. Generate a new
token, set only its digest as `VEND_ADMIN_TOKEN_NEXT_SHA256`, deploy, move every
operator client to the new plaintext, then promote that digest to
`VEND_ADMIN_TOKEN_SHA256`, unset `VEND_ADMIN_TOKEN_NEXT_SHA256`, and deploy
again. Vend compares both configured digests on every request and rejects equal
current/next values. Never place either plaintext token in Vend's environment.
The rate-limit pepper is a separate 256-bit secret; rotating it intentionally
starts fresh counter dimensions and must not reuse an admin or OAuth secret.

Start the data plane with:

```bash
cd apps/vend
DATABASE_URL=postgres://user:pass@127.0.0.1/vend \
MINT_BASE=http://127.0.0.1:8080 \
MINT_API_KEY=mint-secret \
RELAY_BASE=http://127.0.0.1:3000 \
RELAY_KEY=relay-secret \
VEND_CLEARING_ACCOUNT=acct_vend_clearing \
VEND_PROVIDER_PAYOUT_ACCOUNT=acct_provider \
VEND_PLATFORM_PAYOUT_ACCOUNT=acct_platform \
VEND_MODEL_RATES_JSON='{"tensorzero::model_name::openai::gpt-4o-mini":{"input_micros_per_million_tokens":150000,"output_micros_per_million_tokens":600000,"max_input_tokens":128000,"max_output_tokens":16384}}' \
VEND_ADMIN_TOKEN_SHA256="$VEND_ADMIN_TOKEN_SHA256" \
VEND_RATE_LIMIT_PEPPER_HEX="$VEND_RATE_LIMIT_PEPPER_HEX" \
VEND_PUBLIC_BASE_URL=http://127.0.0.1:8090 \
cargo run -p vend-server --bin vend-server
```

It applies its embedded schema before binding and exposes `/healthz`, `/readyz`,
`/metrics`, `POST /v1/chat/completions`, and the control/auth surfaces.
`/readyz` fails closed unless Vend Postgres answers, the configured Mint
clearing account can be read with Vend's Mint credential, and Relay's
dependency-aware `/health` answers. `RELAY_HEALTH_URL` overrides the exact Relay
probe URL for deployments below a gateway prefix. `/metrics` emits only
low-cardinality HTTP and settlement counters with no tenant, user, key, model,
or URL labels; restrict it to the observability network at the ingress layer.

Production HTTP clients refuse redirects and have explicit connect, total,
idle-read, pool, and streaming-lifetime bounds. Request bodies default to 2 MiB,
request ids are validated and propagated as `X-Request-Id`, logs default to
secret-safe JSON fields, and SIGINT/SIGTERM drains in-flight requests for a
bounded grace period. See `.env.example` for every validated override.

For a containerized Vend + Postgres deployment, export the required values from
`.env.example` and run `docker-compose -f docker-compose.vend.yml up --build` at
the repository root. Mint and Relay remain explicit live dependencies; this
stack does not replace either service with a stub.

## Call the data plane

Vend's non-streaming response surface is OpenAI-compatible: it preserves Relay's
JSON body and HTTP status without a Vend success envelope. Every metered request
must include a printable `Idempotency-Key` of at most 128 bytes. Keys are scoped
to the authenticated provisioning key; an exact retry replays the original body,
status, charge id, and settlement headers, while a changed body returns `409`.

```bash
curl --fail-with-body http://127.0.0.1:8090/v1/chat/completions \
  -H "Authorization: Bearer $VEND_PROVISIONING_KEY" \
  -H "Idempotency-Key: chat_01J_example" \
  -H 'Content-Type: application/json' \
  -d '{
    "model":"tensorzero::model_name::openai::gpt-4o-mini",
    "messages":[{"role":"user","content":"Hello"}],
    "max_tokens":256
  }'
```

`stream:true` uses Relay's live response body: Vend preserves the upstream status
and end-to-end headers and forwards each byte chunk unchanged, including the
OpenAI `[DONE]` event. The rate catalog sizes a conservative input-plus-output
hold. Capture happens only after `[DONE]` and a valid final usage event carrying
`usage.tensorzero_cost`, parsed as decimal USD directly into integer
microdollars. A non-2xx response, body transport error, client disconnect,
missing/malformed terminal usage, absent `[DONE]`, negative/sub-micro cost, or a
cost above the hold releases the hold. A stream also has a hard total lifetime
(`VEND_RELAY_MAX_STREAM_SECONDS`, default 900) in addition to its idle-read
timeout; expiry releases the hold. Because headers are already committed
when a stream later proves malformed, the client sees the original 2xx stream
end rather than a replacement JSON 502; the money-safe outcome is still release.

Streaming idempotency keys are deliberately single-use. Vend stores terminal
metadata, never emitted SSE bytes: an in-flight duplicate returns
`idempotency_in_progress`, and every retry after terminal settlement returns
`streaming_idempotency_consumed` instead of replaying a possibly partial stream.
A process crash can leave the key in progress and the usage row pending; the
hold/payout reconcilers are the recovery path. Streaming responses include
`Vend-Charge-Id` and `Vend-Model` up front, but cannot include final cost/price
headers because those values do not exist until after HTTP headers are sent.

Sx0 supports `tenant_reserve` only. Mint reserves the app's configured funding
account and captures the full rated customer charge into `VEND_CLEARING_ACCOUNT`.
Vend first persists an immutable payout plan, then transfers exactly
`provider_cost_micros` to `VEND_PROVIDER_PAYOUT_ACCOUNT`, `tenant_net_micros` to
the tenant's configured Mint account, and `platform_fee_micros` to
`VEND_PLATFORM_PAYOUT_ACCOUNT`. These three amounts must equal the captured
charge. Each leg uses `{charge-idempotency-prefix}:payout:{provider|tenant|platform}`;
durable per-leg progress plus Mint idempotency makes restart replay safe after
ambiguous network or local-commit failures. The three global accounts are
required at startup and clearing/provider/platform must be distinct.

### Reconcile interrupted holds and payouts

Vend commits the cap-checked usage intent before calling Mint. If a process dies
after Mint accepts a hold but before its returned id is attached locally, the
intent remains recoverable: repeating the operation-specific `:hold` key returns
the same Mint hold. Run the one-shot reconciler from a scheduler and alert on a
non-zero exit:

```bash
cd apps/vend
DATABASE_URL=postgres://user:pass@127.0.0.1/vend \
MINT_BASE=http://127.0.0.1:8080 \
MINT_API_KEY=mint-secret \
VEND_RECONCILE_MIN_AGE_SECONDS=1800 \
VEND_RELAY_MAX_STREAM_SECONDS=900 \
VEND_RECONCILE_LEASE_SECONDS=120 \
cargo run -p vend-server --bin vend-reconcile
```

It never calls Relay. For an unattached intent it repeats the idempotent hold
operation and releases it. For an attached intent it reads Mint's authoritative
hold: open holds are released, released holds converge locally, and captured
holds are accepted only when their exact amount matches the immutable payout
plan. Captured plans resume their remaining idempotent provider, tenant, and
platform transfer legs. Bounded Postgres leases use `FOR UPDATE SKIP LOCKED`, so
multiple workers may run concurrently and a crashed worker's batch becomes
eligible after `VEND_RECONCILE_LEASE_SECONDS`. A captured response lost during a
restart is consumed with a deterministic recovery response; inference is never
run twice. Vend refuses an unsafe stale-age configuration: the minimum age must
be at least 60 seconds longer than the Relay stream lifetime, so a legitimate
live SSE request cannot be reclaimed. Alert on any non-zero exit and the
structured `failed` count.
Container operators can run the same one-shot job with
`docker-compose -f docker-compose.vend.yml --profile ops run --rm reconcile`.

## Bootstrap a product

The operator CLI atomically creates a tenant, app, markup policy, app-scoped
provisioning key, and first owner membership. It prints the provisioning key and
owner setup token once; only hashes are persisted. Vend does not send email, so
the operator must deliver the setup token out of band.

```bash
cd apps/vend
DATABASE_URL=postgres://user:pass@127.0.0.1/vend \
VEND_TENANT_NAME='Example Studio' \
VEND_OWNER_EMAIL='owner@example.com' \
VEND_OWNER_DISPLAY_NAME='Example Owner' \
VEND_APP_NAME='Example AI' \
VEND_MINT_ACCOUNT_ID=acct_example \
VEND_MODELS='tensorzero::model_name::openai::gpt-4o-mini' \
VEND_MARKUP_BPS=2000 \
VEND_MIN_FEE_MICROS=0 \
VEND_PLATFORM_FEE_BPS=1000 \
VEND_FUNDING_MODE=tenant_reserve \
VEND_DAILY_CAP_MICROS=50000000 \
cargo run -p vend-server --bin vend-bootstrap
```

`VEND_MINT_ACCOUNT_ID` must name an existing, funded Mint account. In the
bounded bootstrap path it is both the app's hold-funding account and the
tenant-net payout destination; the control-plane app API can assign distinct
app funding accounts while the tenant account remains the payout destination.
Vend metadata and key issuance never create Mint accounts.

`VEND_ZDR=true` means **do not store the inference in Relay observability**: Vend
forces Relay's `tensorzero::dryrun` request field for that app. It does not claim
that the selected upstream model provider has a zero-retention contract; provider
retention remains an operator-owned routing/configuration decision.

`VEND_MODELS`, `VEND_MARKUP_BPS`, `VEND_MIN_FEE_MICROS`,
`VEND_PLATFORM_FEE_BPS`, and a positive `VEND_DAILY_CAP_MICROS` are explicit,
required launch decisions. The bootstrap path supports `tenant_reserve` only.
`VEND_ZDR`, `VEND_KEY_ENV`, and `VEND_PER_END_USER_DAILY_CAP_MICROS` are
optional. Vend records the supplied Mint account id but does not preflight its
existence or funding; operators must verify that in Mint before bootstrap.

## Operate products through the control API

The server exposes separate global-operator and customer control surfaces backed
by Postgres. `/v1/control/*` requires the global `ADMIN_TOKEN`.
`/v1/customer/*` accepts only an expiring, revocable Vend customer session and
derives the tenant from its membership, so customer routes cannot select another
tenant. The OpenAPI 3.1 document is authenticated at
`/v1/control/openapi.json` and `/v1/customer/openapi.json`.

Bootstrap a tenant, first app, pricing policy, and one-time provisioning key:

```bash
curl --fail-with-body http://127.0.0.1:8090/v1/control/tenants \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "tenant_name": "Example Studio",
    "owner_email": "owner@example.com",
    "owner_display_name": "Example Owner",
    "app_name": "Example AI",
    "mint_account_id": "acct_example",
    "model_allowlist": ["tensorzero::model_name::openai::gpt-4o-mini"],
    "zdr": false,
    "markup_policy": {
      "markup_bps": 2000,
      "min_fee_micros": 0,
      "platform_fee_bps": 1000,
      "funding_mode": "tenant_reserve"
    },
    "environment": "test",
    "daily_cap_micros": 50000000,
    "per_end_user_daily_cap_micros": 5000000
  }'
```

The success envelope returns `tenant_id`, `app_id`, `key_id`,
`provisioning_key`, invited owner metadata, and `owner_setup_token`. Both
plaintext secrets are returned once. Key list responses expose only the key id,
non-secret prefix, scope, status, and caps.

### Customer identity and RBAC

Vend owns the customer principal, password, membership, and session records.
One principal may belong to multiple tenants; every session is bound to exactly
one tenant membership. Passwords are Argon2id hashes. Setup and session bearers
contain 256 bits of random entropy, are stored only as SHA-256 digests, compared
in constant time, expire, and can be revoked. Secret-bearing responses use
`Cache-Control: no-store`.

Roles are cumulative and have these exact meanings:

- `viewer`: read its tenant, apps, safe key metadata, and daily usage.
- `admin`: viewer access plus create/enable apps, issue/revoke provisioning keys,
  and provision end-user mappings.
- `owner`: admin access plus invite members, change membership role/status, and
  revoke member sessions. Vend prevents removal of the last active owner.

The global admin or a tenant owner creates an invitation. Vend returns the setup
token once and does not send email; the caller is responsible for secure
delivery. The recipient sets a password, logs into a specific tenant, then uses
the returned session—not an inference provisioning key—on customer routes:

```bash
curl --fail-with-body http://127.0.0.1:8090/v1/auth/setup \
  -H 'Content-Type: application/json' \
  -d '{"setup_token":"'$OWNER_SETUP_TOKEN'","password":"a-long-unique-password"}'

curl --fail-with-body http://127.0.0.1:8090/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"tenant_id":"'$TENANT_ID'","email":"owner@example.com","password":"a-long-unique-password"}'

curl --fail-with-body \
  -H "Authorization: Bearer $VEND_CUSTOMER_SESSION" \
  http://127.0.0.1:8090/v1/customer/apps
```

`VEND_PUBLIC_BASE_URL` is required and supplies the exact GitHub and Google
callbacks (`/v1/auth/github/callback` and `/v1/auth/google/callback`). Configure
either provider by setting its client-id/secret pair; unconfigured provider
routes return `503`. Both use authorization code + PKCE with one-time expiring
server-side state. Google also validates the signed ID token against Google JWKS,
issuer, audience, expiry, nonce, and `email_verified`; GitHub requires the
verified primary email returned by its email API. External login only links an
already invited matching email—there is no open signup—and Vend never persists
provider access or refresh tokens. Native password login remains the fallback.

Native login is throttled by tenant plus normalized email; invitation setup by
the setup token's non-secret lookup prefix; OAuth start by tenant plus provider;
OAuth callback by provider plus the one-time state's non-secret lookup prefix;
and known provisioning-key inference by tenant plus the key's non-secret lookup
prefix. Counters live in Postgres, so the limit
is shared across replicas. Default windows are 5 native logins/minute (5-minute
block), 5 setup attempts/minute (10-minute block), 20 OAuth starts/minute
(5-minute block), 10 callbacks/minute (10-minute block), and 10 known-prefix
key attempts/minute (10-minute block). A rejected request returns `429`,
`Retry-After`, and `Cache-Control: no-store`. Expired-counter cleanup deletes at
most 100 rows per check.

Rotate the current customer session (the old session is revoked in the same
transaction and the new bearer is returned once), or change a native password
while keeping the current session and revoking every other session for that
principal:

```bash
curl --fail-with-body -X POST \
  -H "Authorization: Bearer $VEND_CUSTOMER_SESSION" \
  http://127.0.0.1:8090/v1/customer/session/rotate

curl --fail-with-body -X PATCH \
  -H "Authorization: Bearer $VEND_CUSTOMER_SESSION" \
  -H 'Content-Type: application/json' \
  -d '{"current_password":"old-long-password","new_password":"new-long-password"}' \
  http://127.0.0.1:8090/v1/customer/password
```

Vend writes append-only Postgres audit rows for auth outcomes, every
control/customer operation, hold/settlement/payout transitions, and each
reconciliation result. Streaming records hold and terminal money events only,
never one event per SSE chunk. Session rotation and password changes append
their success event in the same transaction as the credential mutation. Audit
metadata is object-only, capped at 4 KiB, and rejects credential, token,
password, authorization, and request/response-body fields. The database rejects
updates and deletes to the audit table. Auth/provider and general control events
are appended immediately after their operation; operators must alert on audit
append failures because auth/control observations and external Mint operations
cannot be transactionally coupled to the audit insert. Durable usage and payout
rows, plus Mint itself, remain authoritative for recovery.

With the returned ids:

```bash
# Create another app, then list apps and safe key metadata.
curl --fail-with-body http://127.0.0.1:8090/v1/control/tenants/$TENANT_ID/apps \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "name":"Support Copilot",
    "mint_account_id":"acct_example",
    "model_allowlist":["tensorzero::model_name::openai::gpt-4o-mini"],
    "zdr":false,
    "markup_policy":{"markup_bps":1500,"min_fee_micros":0,"platform_fee_bps":1000,"funding_mode":"tenant_reserve"}
  }'
curl --fail-with-body -H "Authorization: Bearer $ADMIN_TOKEN" \
  "http://127.0.0.1:8090/v1/control/tenants/$TENANT_ID/apps"
curl --fail-with-body -H "Authorization: Bearer $ADMIN_TOKEN" \
  "http://127.0.0.1:8090/v1/control/apps/$APP_ID/keys"

# Issue another key; capture the one-time plaintext from the success envelope.
curl --fail-with-body http://127.0.0.1:8090/v1/control/apps/$APP_ID/keys \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"environment":"live","daily_cap_micros":100000000}'

# Revoke a key or stop/start all new inference for an app.
curl --fail-with-body -X DELETE \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  "http://127.0.0.1:8090/v1/control/keys/$KEY_ID"
curl --fail-with-body -X PATCH \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"enabled":false}' \
  "http://127.0.0.1:8090/v1/control/apps/$APP_ID/enabled"

# Customer-visible billed spend for one UTC day (microdollars).
curl --fail-with-body -H "Authorization: Bearer $ADMIN_TOKEN" \
  "http://127.0.0.1:8090/v1/control/apps/$APP_ID/usage?utc_day=2026-08-22"
```

Provision known customer references for attribution using an existing real Mint
account id:

```bash
curl --fail-with-body http://127.0.0.1:8090/v1/control/apps/$APP_ID/end-users \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"external_ref":"customer-123","mint_account_id":"acct_customer_123"}'
```

Unknown `Vend-End-User` values are rejected. End-user-funded apps never fall
back to charging the app account, and end-user-scoped keys resolve their bound
typed id before accepting a matching external reference. New app creation is
currently limited to `tenant_reserve`; the end-user mapping is retained for
attribution and a future audited `end_user_reserve` launch.

The remaining product gaps include BYOK/end-user funding and broader operator
automation. The atomic request claim prevents concurrent duplicate execution,
and `vend-reconcile` closes pre-attachment, attached-hold, lost local-capture,
and incomplete-payout crash windows by converging against Mint.
Non-tenant funding modes are rejected until their settlement paths are audited.
Operators no longer need
hand-written SQL for the supported tenant, app, key, end-user, and spend flows.

## Client and deployment surfaces

- Exact live curl onboarding: [`docs/QUICKSTART.md`](docs/QUICKSTART.md)
- Release, backup/restore, rotation, reconciliation, and alert runbook:
  [`docs/OPERATIONS.md`](docs/OPERATIONS.md)
- TypeScript client and executable example: [`sdks/typescript`](sdks/typescript)
- Python client and executable example: [`sdks/python`](sdks/python)
- Docker Compose and Kubernetes/Prometheus templates: [`deploy`](deploy)
- No-mock deployed-system proof harness: [`scripts/smoke-live.sh`](scripts/smoke-live.sh)

The smoke harness requires actual Vend, Postgres, Mint, and Relay coordinates.
It fails on missing dependencies, exercises native customer login, performs one
settled inference, and verifies an exact idempotent replay. It is not run against
fakes and a pass is evidence only for the deployed path tested at that time.
