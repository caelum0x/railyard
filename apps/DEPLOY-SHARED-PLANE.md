# Railyard shared plane: co-deploy on one box

How the Railyard services run side by side on the single products box, behind
one **Caddy** front door and **one shared Postgres** (one database and role per
service). There is no ClickHouse: Relay stores its observability data in
Postgres too.

The deploy kit that implements this doc lives in the `infra-box` repo:

| What | Where (infra-box) |
|---|---|
| Compose file (source of truth for ports and profiles) | `stacks/railyard/docker-compose.yml` |
| Shared Postgres 16 + pg_cron, per-app DB/role init | `stacks/postgres/`, `stacks/postgres/init/10-app-dbs.sh` |
| Env templates (seeded to `/etc/products/<svc>.env`) | `env/<svc>.env.example` |
| Caddy site blocks (commented until DNS exists) | `caddy/Caddyfile` |
| Serial image builds | `build/build-railyard.sh` |
| Installer | `install.sh railyard [svc...]` |

Each service owns its own database and **runs its schema migrations at process
startup**, so a deploy is "build image, fill env, `docker compose up -d`". There
is no separate migration step.

Every service is behind a compose **profile**, so a plain `docker compose up`
starts nothing. Enable services explicitly:

```bash
sudo ./install.sh railyard                                   # rago mint relay
sudo ./install.sh railyard vend turnstile game-economy        # next wave
sudo ./install.sh railyard abacus relay-ui                    # spend caps + operator console
```

---

## 1. Topology

```
                     Caddy (:80/:443, TLS)  <svc>.$RAILYARD_DOMAIN
                                   |
   internet  ---->   rago.      -> 127.0.0.1:3300
                     relay.     -> 127.0.0.1:3100
                     mint.      -> 127.0.0.1:3400
                     vend.      -> 127.0.0.1:3600
                     toolgate.  -> 127.0.0.1:4200
                     ledgerline. -> 127.0.0.1:4300
                     context-passport. -> 127.0.0.1:4400
                     turnstile. -> 127.0.0.1:3700
                     game.      -> 127.0.0.1:3800
                     abacus.    -> 127.0.0.1:3900
                     relay-ui.  -> 127.0.0.1:4000  (basic_auth; SSH tunnel preferred)
                                   |
        docker network "products-net" (services call each other by name)
                                   |
   rago:3000  relay:3000  mint:8080  vend:8090  toolgate:8130  ledgerline:8093
   context-passport:8095
   turnstile:8101  game-economy:8102  abacus:8100  relay-ui:4000
                                   |
                     postgres:5432 (stacks/postgres; host 127.0.0.1:5433)
       databases: rago  mint  relay  vend  switchyard  ledgerline  context_passport  turnstile  game_economy  abacus
```

### Port map

Every host port binds `127.0.0.1` only; Caddy is the only public listener.
Containers keep their built-in default port; the compose file remaps the host
side so nothing collides with Menivor (8080) or OpenJev (3000).

| Service | Profile | Host port | Container port | Health | Database |
|---|---|---|---|---|---|
| rago (API + GraphQL + billing worker) | `railyard` | 3300 | 3000 | `GET /health` | `rago` |
| relay (LLM gateway) | `railyard` | 3100 | 3000 | `GET /health` | `relay` |
| mint (ledger, holds, receipts) | `railyard` | 3400 | 8080 | `GET /healthz`, `/readyz` | `mint` |
| vend (white-label LLM API) | `vend` | 3600 | 8090 | `GET /readyz` | `vend` |
| toolgate / Switchyard (tool rail + MCP) | `toolgate` | 4200 | 8130 | `GET /healthz` | `switchyard` |
| ledgerline (revenue vs AI cost margin lens) | `ledgerline` | 4300 | 8093 | `GET /healthz`, `/readyz` | `ledgerline` |
| context-passport / Identity Lock (locked brand/character identity) | `context-passport` | 4400 | 8095 | `GET /healthz`, `/readyz` | `context_passport` |
| turnstile (AITP-402 paid tools) | `turnstile` | 3700 | 8101 | `GET /healthz` | `turnstile` |
| game-economy (game storefront, IAP credits) | `game-economy` | 3800 | 8102 | `GET /healthz` | `game_economy` |
| abacus (per-agent LLM spend caps) | `abacus` | 3900 | 8100 | `GET /healthz`, `/readyz` | `abacus` |
| relay-ui (Relay operator console) | `relay-ui` | 4000 | 4000 | `GET /health` | reads `relay` |

On-box consumers (Menivor, OpenJev, FlowBlast if co-hosted) use the in-network
names: `http://rago:3000`, `http://relay:3000`, `http://mint:8080`,
`http://abacus:8100`, `http://game-economy:8102`. Off-box consumers (Rally on
the Oracle micro) use the public Caddy hosts.

---

## 2. One Postgres, one database per service

`stacks/postgres` runs Postgres 16 with pg_cron (Relay's Postgres
observability backend needs it). On first init, and again on every
`install.sh postgres`, `init/10-app-dbs.sh` creates one login role and one
owned database per service from the passwords in `/etc/products/postgres.env`.
An empty password skips that database, so next-wave databases only appear once
you enable the service.

| Password var (postgres.env) | Role | Database | Used by |
|---|---|---|---|
| `RAGO_DB_PASSWORD` | `rago_app` | `rago` | rago |
| `MINT_DB_PASSWORD` | `mint_app` | `mint` | mint |
| `RELAY_DB_PASSWORD` | `relay_app` | `relay` | relay, relay-ui |
| `VEND_DB_PASSWORD` | `vend_app` | `vend` | vend |
| `TOOLGATE_DB_PASSWORD` (role `switchyard_app`, db `switchyard`) | `switchyard_app` | `switchyard` | toolgate |
| `LEDGERLINE_DB_PASSWORD` | `ledgerline_app` | `ledgerline` | ledgerline |
| `CONTEXT_PASSPORT_DB_PASSWORD` | `context_passport_app` | `context_passport` | context-passport |
| `TURNSTILE_DB_PASSWORD` | `turnstile_app` | `turnstile` | turnstile |
| `GAME_ECONOMY_DB_PASSWORD` | `game_economy_app` | `game_economy` | game-economy |
| `ABACUS_DB_PASSWORD` | `abacus_app` | `abacus` | abacus |

Separate databases keep each service's embedded migrator the only owner of its
DDL, so migration histories never collide.

### Migrations at startup

| Service | Mechanism | Trigger |
|---|---|---|
| rago | embedded sqlx migrator (`rago-db`) | `DATABASE_URL` set |
| mint | migrations at server start (production mode) | `DATABASE_URL` set |
| relay | Postgres migrations at gateway start (observability, API keys, provider credentials) | `TENSORZERO_POSTGRES_URL` set |
| vend | migrations at server start | `DATABASE_URL` set |
| toolgate | explicit one-off step (`switchyard-server migrate`), not run on every boot | run once before first `up -d`, and again after a schema-changing deploy |
| ledgerline | embedded `sqlx::migrate!()` at server start | `DATABASE_URL` set |
| context-passport | embedded migration at server start | `DATABASE_URL` set |
| turnstile | migrations at server start | `TURNSTILE_DATABASE_URL` set |
| game-economy | migrations at server start | `GAME_DATABASE_URL` set |
| abacus | migrated Postgres product store (policy, receipts, open holds) | `ABACUS_DATABASE_URL` set |

---

## 3. Env matrix

The templates in `infra-box/env/` are the complete lists; this table names the
required keys. `install.sh` refuses to start a service while its env file still
contains `replace` or `example.com`.

| Service | Env file | Required keys |
|---|---|---|
| rago | `rago.env` | `DATABASE_URL`, `RAGO_API_ADDR=0.0.0.0:3000`, `RAGO_JWT_SECRET` (refuses to boot without it in release), `RAGO_ENCRYPTION_KEY` |
| relay | `relay.env` | `TENSORZERO_POSTGRES_URL`, R2 object storage (`R2_BUCKET_NAME`, `R2_S3_ENDPOINT`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`), `RELAY_CREDENTIALS_KEY`. Provider keys (`GROQ_API_KEY`, `OPENROUTER_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`): any subset boots; a provider without a key answers 503 `provider_not_configured`. Optional: `RELAY_OPERATOR_KEY_IDS`, Rago cost feed (`RELAY_COST_FEED_URL=http://rago:3000/api/v1/events`, `RELAY_COST_FEED_FORMAT=rago`, `RELAY_COST_FEED_BEARER` or `RELAY_RAGO_ROUTES_JSON`) |
| mint | `mint.env` | `MINT_BIND_ADDR=0.0.0.0:8080`, `DATABASE_URL`, `MINT_API_KEY`, `MINT_SIGNING_KEY`, `MINT_PAYER_KEYS_JSON` |
| vend | `vend.env` | `DATABASE_URL`, `MINT_BASE`, `MINT_API_KEY`, `RELAY_BASE=http://relay:3000`, `RELAY_KEY`, payout accounts, `VEND_MODEL_RATES_JSON`, `VEND_ADMIN_TOKEN_SHA256` |
| toolgate | `toolgate.env` | `SWITCHYARD_PUBLIC_URL`, `SWITCHYARD_DATABASE_URL`, `SWITCHYARD_KEK`/`SWITCHYARD_KEK_VERSION` (`switchyard-server gen-kek`), `SWITCHYARD_JWT_SEED`, `SWITCHYARD_ADMIN_TOKEN`, `MINT_SERVER_URL=http://mint:8080`, `MINT_API_KEY`, `SWITCHYARD_MINT_REVENUE_ACCOUNT` (a `Provider`-kind Mint account, `POST /v1/accounts`). Optional: `RELAY_DATABASE_URL` (defaults to `SWITCHYARD_DATABASE_URL`), `RAGO_URL`/`RAGO_API_KEY` (unset = usage events stay pending), `SWITCHYARD_RELAY_URL`/`SWITCHYARD_SCRIBASE_URL`/`SWITCHYARD_RALLY_URL`/`SWITCHYARD_MENIVOR_URL` (own-product tool base URLs; unset tools answer `byo_required`) |
| ledgerline | `ledgerline.env` | `DATABASE_URL`, `MINT_BASE=http://mint:8080`, `MINT_API_KEY`, `LEDGERLINE_API_KEY`, `LEDGERLINE_ACCOUNT_POLICY_JSON` (allowlisted `revenue_accounts`/`cogs_accounts`/`provider_accounts` plus a `customers` map — all real Mint account ids, `POST /v1/accounts`; an event naming an account outside this policy gets `403` before any Mint call) |
| context-passport | `context-passport.env` | `DATABASE_URL`, `PASSPORT_API_KEY`, `MINT_BASE_URL=http://mint:8080` (discovers receipt-verification keys from Mint's `/.well-known/mint-verification-keys.json`; alternative: `MINT_RECEIPT_PUBLIC_KEYS` directly), `RELAY_BASE=http://relay:3000`, `RELAY_API_KEY` (a real Relay key, `gateway --create-api-key`). Optional: `PASSPORT_OWNER_KEYS` (per-owner tenant keys) |
| turnstile | `turnstile.env` | `MINT_SERVER_URL`, `TURNSTILE_API_KEY`, `TURNSTILE_PAYEE`, `TURNSTILE_DATABASE_URL`, `TURNSTILE_ADMIN_TOKEN`; host file `/srv/railyard/turnstile/tools.json` |
| game-economy | `game-economy.env` | `RAILYARD_MINT_URL`, `MINT_API_KEY`, `GAME_REVENUE_ACCOUNT`, `GAME_DATABASE_URL`, `GAME_AUTH_SECRET`, `GAME_SERVER_SECRET`, `GAME_ADMIN_TOKEN`; host file `/srv/railyard/game-economy/catalog.json` |
| abacus | `abacus.env` | `ABACUS_DATABASE_URL`, `ABACUS_API_KEY`, `MINT_SERVER_URL=http://mint:8080`, `MINT_API_KEY`, `UPSTREAM_BASE_URL=http://relay:3000`, `UPSTREAM_PATH=/openai/v1/chat/completions`, `ABACUS_PROVIDER=relay`; optional `ABACUS_ADMIN_API_KEY` (operator API). Host files `/srv/railyard/abacus/agents.json` (required) and `pricing.json` (Relay model prices; unpriced models fail with `model_not_priced`) |
| relay-ui | `relay-ui.env` | `TENSORZERO_POSTGRES_URL` (relay database), `TENSORZERO_API_KEY` (a Relay key created with `gateway --create-api-key`); `TENSORZERO_GATEWAY_URL=http://relay:3000` is pinned in compose |

Relay's gateway auth is on (`config/tensorzero.toml` `[gateway.auth]`), so
every caller, including Abacus pass-through traffic and the console, needs a
Relay API key. Abacus forwards the caller's own `Authorization: Bearer` key to
Relay.

Consumer-side env (Rally, Menivor, OpenJev, Scribase, FlowBlast) is listed in
the matching `infra-box/env/*.env.example` sections and in
[`docs/CONSUMERS.md`](../docs/CONSUMERS.md).

---

## 4. Caddy front door

`infra-box/caddy/Caddyfile` has one commented block per service under the
"Railyard" section. `RAILYARD_DOMAIN` (caddy.env) must be an apex zone, e.g.
`ereboslab.com`, giving `rago.ereboslab.com`: Cloudflare's free Universal SSL
covers one subdomain level only. Uncomment the blocks for the services you
enabled, then `sudo ./install.sh caddy`; it refuses to load a block whose
`{$VAR}` is empty.

- Streaming services (relay, vend, turnstile, abacus, relay-ui) use
  `flush_interval -1` and long read timeouts.
- relay-ui has no login of its own and can create API keys and store provider
  keys. Its block requires `basic_auth` (`RELAY_UI_USER`,
  `RELAY_UI_PASSWORD_HASH` from `caddy hash-password`). The preferred access is
  an SSH tunnel: `ssh -L 4000:127.0.0.1:4000 <box>`.

The apps use absolute paths (`/api/v1/events`, `/v1/holds`, `/v1/proxy`, ...),
so subdomains are the supported layout; path-prefix routing would need a
`handle_path` strip per app.

---

## 5. Builds

Images are built on the box from a checkout at `/srv/src/railyard`
(`RAILYARD_SRC`), one service at a time (`build/build-railyard.sh` holds a lock
so a Rust build never overlaps another heavy build). Every Dockerfile uses
multi-arch bases and compiles from source, so the same file builds natively on
amd64 or arm64.

| Service | Build context | Dockerfile | Context trimmed by |
|---|---|---|---|
| rago | `apps/rago` | `Dockerfile` | none (small dir; a clean checkout has no `target/`) |
| mint | `apps/mint` | `Dockerfile` | `apps/mint/.dockerignore` |
| relay | `apps/relay` | `Dockerfile` | `apps/relay/.dockerignore` |
| relay-ui | `apps/relay` | `ui/Dockerfile` | `apps/relay/ui/Dockerfile.dockerignore` |
| vend | repo root | `apps/vend/Dockerfile` | `apps/vend/Dockerfile.dockerignore` (vend + mint crates) |
| turnstile | repo root | `apps/turnstile/Dockerfile` | `apps/turnstile/Dockerfile.dockerignore` (turnstile + agent-runtime) |
| game-economy | repo root | `apps/game-economy/Dockerfile` | `apps/game-economy/Dockerfile.dockerignore` (game-economy + mint) |
| abacus | repo root | `apps/abacus/Dockerfile` | `apps/abacus/Dockerfile.dockerignore` (abacus + agent-runtime) |

The repo-root images path-depend on sibling workspaces, which is why their
context is the repo root. BuildKit reads the per-Dockerfile
`<Dockerfile>.dockerignore`, so each root build uploads only the workspaces it
compiles.

Memory limits in compose: rago and relay 512 MB, relay-ui 384 MB, every other
service 256 MB.

---

## 6. Deploy order and smoke test

1. `sudo ./install.sh env`, fill `/etc/products/{postgres,rago,mint,relay}.env`.
2. `sudo ./install.sh railyard` (brings up Postgres, builds and starts rago,
   mint, relay; each migrates its database on boot).
3. Create Relay API keys (one per consumer product, one for relay-ui):
   `docker compose -p railyard exec relay gateway --config-file /app/config/tensorzero.toml --create-api-key`
4. Optional services: fill their env files and host files, add their DB
   passwords to `postgres.env`, then `sudo ./install.sh railyard <svc...>`.
5. Uncomment the matching Caddy blocks and `sudo ./install.sh caddy`.
6. Health checks from the box:
   ```bash
   curl -fsS http://127.0.0.1:3300/health    # rago
   curl -fsS http://127.0.0.1:3100/health    # relay
   curl -fsS http://127.0.0.1:3400/healthz   # mint
   curl -fsS http://127.0.0.1:3900/healthz   # abacus (if enabled)
   curl -fsS http://127.0.0.1:4000/health    # relay-ui (if enabled)
   ```
7. Seed Rago catalogs per consumer (see `apps/rago/seeds/README.md`):
   ```bash
   cd apps/rago/seeds
   RAGO_API_URL=https://rago.<apex>/api/v1 RAGO_API_KEY=<autogtm-key>      ./import.sh autogtm.json
   RAGO_API_URL=https://rago.<apex>/api/v1 RAGO_API_KEY=<menivor-key>      ./import.sh menivor.json
   RAGO_API_URL=https://rago.<apex>/api/v1 RAGO_API_KEY=<opentypesafe-key> ./import.sh opentypesafe.json
   ```

---

## 7. Notes

- **Mint is the authoritative money ledger.** Abacus holds, game-economy
  purchases, vend settlements and turnstile payments all settle against it;
  keep `MINT_API_KEY` and the Mint account ids in `agents.json`,
  `GAME_REVENUE_ACCOUNT` and the vend payout accounts in sync with real Mint
  accounts.
- **Secrets:** never commit a filled env file. Generate with
  `openssl rand -hex 32`. Rotate anything that was ever pasted in chat.
- **Backups:** the box's nightly restic backup (`infra-box/backup/backup.sh`)
  includes a `pg_dumpall` of the shared Postgres, so every Railyard database is
  covered.
