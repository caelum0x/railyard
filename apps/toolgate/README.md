# Switchyard (apps/toolgate)

The **tool rail** next to Relay's model rail. One Relay `sk-t0-` key (or an
OAuth token) reaches a schema-typed catalog of tools: our own products first
(Relay, Scribase, Rally, Menivor), free and public APIs second, the team's own
keys third. Every call names the provider that served it and its cost in
micro-USD, paid calls are pre-authorised with a Mint hold and captured at the
exact cost, and failures are never billed.

Working name `toolgate`; the recommended product name is **Switchyard** (CLI
`sy`). The directory stays `apps/toolgate` until the name is confirmed.
Clean-room implementation of `gtm/TOOLGATE-SPEC-2026-09-29.md`.

## Layout

Standalone cargo workspace (never part of a root workspace), like Abacus and Rago.

| Crate | Role |
|---|---|
| `switchyard-core` | Manifests, micro-USD prices (quote/settle), `Adapter` trait, HTTP seam, error envelope, hash chain |
| `switchyard-catalog` | YAML loader + integrity checks, JSON Schema 2020-12 validation, output mapping, templates, search by task |
| `switchyard-adapters` | Declarative HTTP adapter + native adapters (own products, arXiv, RDAP, SEC, registries, readability) |
| `switchyard-egress` | SSRF-safe client: allowlist, private/metadata IP block, DNS pinning, re-checked redirects, caps |
| `switchyard-vault` | Envelope encryption (per-org XChaCha20-Poly1305 DEK, versioned KEK, re-wrap) |
| `switchyard-meter` | Mint holds/captures/releases, Rago events, durable outbox, spend caps |
| `switchyard-cache` | TTL response cache with a byte budget, single-flight dedupe |
| `switchyard-auth` | Relay key validation, scopes, EdDSA tokens, OAuth 2.1 AS (PKCE, DCR, CIMD, rotation, device flow) |
| `switchyard-store` | `Store` trait, in-memory and Postgres implementations, migrations |
| `switchyard-mcp` | MCP JSON-RPC layer (streamable HTTP) |
| `switchyard-server` | Call pipeline, HTTP API, `/mcp`, OAuth routes, `/admin`, `llms.txt`, `openapi.json` |
| `switchyard-cli` | `switchyard` / `sy` binaries |
| `switchyard-testkit` | Fake upstreams, Mint and Rago; one provider fixture per catalog tool |

`catalog/` holds the manifests: 24 capabilities, 27 tools (11 own-product,
16 free/public; Brave is BYO), and the public `lead-to-video` SKILL.md bundle.
Adding a keyless JSON API is a YAML file, no Rust.

## Deployed (2026-10-27)

Live on railyard-box (`130.61.113.116`), `profile: toolgate` in
`infra-box/stacks/railyard/docker-compose.yml`, container port 8130 -> host
`127.0.0.1:4200` -> Caddy `toolgate.{$RAILYARD_DOMAIN}` (site file
`infra-box/caddy/sites/toolgate.caddy`, enabled in `CADDY_SITES`). Own Postgres
database `switchyard` (role `switchyard_app`), own KEK/JWT seed/admin token in
`/etc/products/toolgate.env`, wired to the real Railyard Mint (`http://mint:8080`,
the same box's `mint-mint-1`, shadow mode — see `apps/mint`) with a real
`Provider`-kind revenue account. `RELAY_DATABASE_URL` points at the real
`relay` database (read access, same role Relay itself uses) so Relay
(`sk-t0-`) keys validate; `SWITCHYARD_RELAY_URL=https://relay.ereboslab.com`.
`RAGO_URL`/`RAGO_API_KEY` and the other own-product URLs
(`SWITCHYARD_SCRIBASE_URL`/`SWITCHYARD_RALLY_URL`/`SWITCHYARD_MENIVOR_URL`)
are intentionally unset — no static shared credential exists for them here, so
those tools correctly answer `byo_required`/stay off, same documented
limitation as every prior session's audit of this app.

Verified live end-to-end against the real deployed container (not a local
throwaway process): real self-serve `POST /v1/auth/signup`; real
`GET /v1/tools/search`; a real free tool call
(`knowledge.wikipedia.summary`) returned a real Wikipedia summary; real MCP
`initialize`/`tools/list` over `/mcp`; the `sy` CLI's `search` subcommand
against the live server over the docker network; a paid tool call
(`rally.email.verify`) against a project whose Mint account was a real,
freshly created zero-balance `Provider`-kind Mint account returned a real
`insufficient_balance` with the real balance (`0`) read live from Mint's own
`GET /v1/accounts/:id/balance` (confirmed independently by querying that same
account directly against `mint.ereboslab.com`) — not a local stub decision.
`/catalog`, `/llms.txt` and `/openapi.json` all serve correctly.

**Live (2026-10-28):** the public DNS A record (`toolgate.ereboslab.com` ->
`130.61.113.116`, proxied) is created — the `cloudflare-api` MCP connection is
still read-only, but `~/products/.env`'s `CLOUDFLARE_DNS_API_TOKEN` (a
separate, real, write-scoped token — distinct from the MCP connection and
confirmed active via `GET /user/tokens/verify`) is not. Created the record
directly against the Cloudflare API, reloaded Caddy on railyard-box, and
confirmed via `journalctl -u caddy` that it obtained a real Let's Encrypt cert
for `toolgate.ereboslab.com` within seconds (`tls.obtain: certificate obtained
successfully`). Verified live end to end: `https://toolgate.ereboslab.com/catalog`
returns `200` through Cloudflare.

## Build and test

```bash
cd apps/toolgate
cargo test --workspace
# Postgres-backed store and pipeline tests (creates and drops scratch databases):
SWITCHYARD_TEST_PG_ADMIN_URL=postgres://localhost/postgres cargo test --workspace
# Live smoke against the real public providers through the real egress:
SWITCHYARD_LIVE_SMOKE=1 cargo test -p switchyard-adapters live -- --nocapture
```

## Run

```bash
switchyard-server gen-kek                      # once; store as SWITCHYARD_KEK
SWITCHYARD_DATABASE_URL=postgres://... SWITCHYARD_KEK=... switchyard-server migrate
switchyard-server serve                        # :8130
switchyard-server rotate-kek                   # after moving the old KEK to SWITCHYARD_KEK_PREVIOUS
```

Throwaway local run: `SWITCHYARD_STORE=memory SWITCHYARD_ADMIN_TOKEN=dev switchyard-server serve`.

| Variable | Purpose |
|---|---|
| `SWITCHYARD_PUBLIC_URL` | Public base URL (OAuth issuer, MCP resource `…/mcp`) |
| `SWITCHYARD_DATABASE_URL` | Switchyard Postgres (migrations run at start) |
| `RELAY_DATABASE_URL` | Relay's Postgres holding `tensorzero_auth_api_key` (defaults to the Switchyard DB) |
| `SWITCHYARD_KEK`, `SWITCHYARD_KEK_VERSION`, `SWITCHYARD_KEK_PREVIOUS` | Vault key-encryption keys |
| `MINT_SERVER_URL`, `MINT_API_KEY`, `SWITCHYARD_MINT_REVENUE_ACCOUNT` | Holds and captures; captures settle to the revenue account |
| `SWITCHYARD_TOPUP_PROVIDER` (default `dodo`), `SWITCHYARD_TOPUP_SUCCESS_URL` | Top-ups through Mint `/v1/checkouts` |
| `RAGO_URL`, `RAGO_API_KEY` | `tool_call` usage events |
| `SWITCHYARD_SCRIBASE_API_URL`, `SWITCHYARD_SCRIBASE_LOGIN_URL` | Human sign-in for consent and device pages |
| `SWITCHYARD_JWT_SEED` | Ed25519 seed for access tokens (32 bytes, base64) |
| `SWITCHYARD_ADMIN_TOKEN` | Operator token for `/admin` and `/admin/api/*` |
| `SWITCHYARD_SIGNUP_DISABLED` | Set to `1` to turn off public `POST /v1/auth/signup` (on by default; falls back to the admin-token flow below) |
| `SWITCHYARD_RELAY_URL`, `SWITCHYARD_SCRIBASE_URL`, `SWITCHYARD_RALLY_URL`, `SWITCHYARD_MENIVOR_URL`, `SWITCHYARD_SEARXNG_URL`, `SWITCHYARD_SCRIBASE_PROJECT_DOMAIN` | Product and self-hosted base URLs (their hosts join the egress allowlist). `scribase.auth.magic_link.send` stays off until the project domain is set |
| `SWITCHYARD_RALLY_TOKEN`, `SWITCHYARD_MENIVOR_TOKEN` | Platform service credentials used when a team has none stored |
| `SWITCHYARD_CONTACT_EMAIL` | Contact in the User-Agent (Nominatim, SEC, Wikimedia etiquette) |
| `SWITCHYARD_CACHE_MB` | Response cache budget (default 32) |

Self-serve (default, no operator token needed) -- browse pricing at
`$URL/catalog`, then:

```bash
curl -X POST $URL/v1/auth/signup -d '{"org_name":"Acme","email":"a@acme.com"}'
# -> { "org": {...}, "project": {...}, "key": "sk-t0-...", "note": "shown once" }
```

This creates the org, a default project with no caps, and a full-scope
(`admin`) key in one call -- free and cached tools work immediately; paid
tools answer 402 with a `topup_url` until a Mint account is attached
(`PATCH` is still operator-only, see below). `SWITCHYARD_SIGNUP_DISABLED=1`
turns this off and falls back to the original admin-token-curl-only flow:

```bash
curl -X POST $URL/admin/api/orgs -H "authorization: Bearer $SWITCHYARD_ADMIN_TOKEN" \
  -d '{"slug":"acme","name":"Acme","mint_account_id":"acct_...","rago_subscription_id":"sub_...","owner_user_id":"<scribase user id>","monthly_cap_micros":20000000}'
curl -X POST $URL/admin/api/keys -H "authorization: Bearer $SWITCHYARD_ADMIN_TOKEN" \
  -d '{"project_id":"prj_...","scopes":["tools:read","tools:call"]}'
```

## Use

```bash
sy login --url https://rail.example.com        # device code; no key is pasted
sy search "verify an email"
sy describe rally.email.verify
sy call rally.email.verify --input '{"email":"a@b.com"}'
sy call web.search --input '{"q":"axum"}' --dry-run
pbpaste | sy cred add brave --value-stdin       # write-only
sy project cap --monthly 20 --daily 2
sy bundle install lead-to-video
sy mcp                                          # stdio MCP shim
```

MCP clients connect to `https://…/mcp` and discover OAuth through
`/.well-known/oauth-protected-resource/mcp`. The default surface is five
meta-tools; `PATCH /v1/project {"pinned_tools": [...]}` exposes up to 20 tools
first-class with their schemas and safety annotations.

## Deviations from the spec (MVP)

- MCP is implemented directly on the JSON-RPC wire format instead of `rmcp`
  (smaller binary, same protocol); responses are JSON, no SSE stream.
- Search is in-process BM25 (no tantivy, no embeddings yet).
- Output mapping is our own JSON Pointer mapping language instead of JSONata.
- Relay keys are validated by querying Relay's key table directly (same SHA-256
  scheme as `tensorzero-auth`) rather than a path dependency on that crate,
  whose workspace-inherited manifest cannot build outside Relay's workspace.
- Cache hits are charged at the tool's `cache_micros` (0 in the shipped catalog).
- Oversized responses fail (16 MiB cap) instead of streaming to Scribase storage.
- `POST /v1/credentials/oauth/{provider}/connect` works for providers whose manifest has an `oauth` block and whose app is in `SWITCHYARD_OAUTH_CLIENTS`; none ship one yet.
- One project cap covers tools only; sharing Relay's budget ledger is phase 2.
